use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::config::{SEMANTIC_MERGE_FAN_IN, SEMANTIC_SPOOL_RUN_BYTES};
use crate::{PocError, PocResult};

use super::record::{SemanticRecord, MAX_KEY_BYTES, MAX_RECORD_BYTES};

const RUN_MAGIC: &[u8; 8] = b"MPLARUN1";
const IO_BUFFER_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpoolStats {
    pub records_in: u64,
    pub records_out: u64,
    pub initial_runs: u64,
    pub merge_passes: u64,
    pub max_fan_in: usize,
    pub maximum_buffer_bytes: usize,
    pub bytes_written: u64,
    pub peak_open_files: usize,
}

#[derive(Debug)]
struct Entry {
    key: Vec<u8>,
    payload: Vec<u8>,
}

pub struct BoundedSpool {
    root: PathBuf,
    registry: PathBuf,
    memory_limit: usize,
    buffered_bytes: usize,
    entries: Vec<Entry>,
    next_run: u64,
    stats: SpoolStats,
}

impl BoundedSpool {
    pub fn new(root: PathBuf, memory_limit: usize) -> PocResult<Self> {
        if memory_limit == 0 || memory_limit > SEMANTIC_SPOOL_RUN_BYTES {
            return Err(PocError::Integrity(
                "semantic spool memory limit is outside fixed bounds".to_owned(),
            ));
        }
        std::fs::create_dir(&root)
            .map_err(|error| PocError::io("create semantic spool", &root, error))?;
        let registry = root.join("runs.current");
        File::create(&registry)
            .and_then(|file| file.sync_all())
            .map_err(|error| PocError::io("create semantic spool registry", &registry, error))?;
        Ok(Self {
            root,
            registry,
            memory_limit,
            buffered_bytes: 0,
            entries: Vec::new(),
            next_run: 0,
            stats: SpoolStats::default(),
        })
    }

    pub fn push_record(&mut self, record: SemanticRecord) -> PocResult<()> {
        self.push(record.key_digest()?.to_vec(), record.encode()?)
    }

    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub fn push(&mut self, key: Vec<u8>, payload: Vec<u8>) -> PocResult<()> {
        validate_entry(&key, &payload)?;
        let entry_bytes = key
            .len()
            .checked_add(payload.len())
            .and_then(|value| value.checked_add(8))
            .ok_or_else(|| PocError::Integrity("semantic spool size overflow".to_owned()))?;
        if entry_bytes > self.memory_limit {
            return Err(PocError::Integrity(
                "single semantic spool entry exceeds its memory run".to_owned(),
            ));
        }
        if !self.entries.is_empty()
            && self
                .buffered_bytes
                .checked_add(entry_bytes)
                .is_none_or(|value| value > self.memory_limit)
        {
            self.flush_run()?;
        }
        self.buffered_bytes += entry_bytes;
        self.stats.maximum_buffer_bytes = self.stats.maximum_buffer_bytes.max(self.buffered_bytes);
        self.stats.records_in = self.stats.records_in.saturating_add(1);
        self.entries.push(Entry { key, payload });
        Ok(())
    }

    pub fn finish(mut self) -> PocResult<SortedSpool> {
        if !self.entries.is_empty() {
            self.flush_run()?;
        }
        if self.stats.initial_runs == 0 {
            let path = self.allocate_run_path("empty");
            write_run(&path, std::iter::empty())?;
            append_registry(&self.registry, &path)?;
            self.stats.initial_runs = 1;
            self.stats.peak_open_files = self.stats.peak_open_files.max(1);
        }

        let mut run_count = self.stats.initial_runs;
        let mut registry = self.registry.clone();
        while run_count > 1 {
            let next_registry = self.root.join(format!(
                "runs.pass-{:04}",
                self.stats.merge_passes.saturating_add(1)
            ));
            let mut source =
                BufReader::new(File::open(&registry).map_err(|error| {
                    PocError::io("open semantic run registry", &registry, error)
                })?);
            let mut destination = BufWriter::new(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&next_registry)
                    .map_err(|error| {
                        PocError::io("create semantic merge registry", &next_registry, error)
                    })?,
            );
            let mut output_count = 0_u64;
            loop {
                let mut group = Vec::with_capacity(SEMANTIC_MERGE_FAN_IN);
                for _ in 0..SEMANTIC_MERGE_FAN_IN {
                    let mut line = String::new();
                    if source.read_line(&mut line).map_err(|error| {
                        PocError::io("read semantic run registry", &registry, error)
                    })? == 0
                    {
                        break;
                    }
                    let filename = line.trim_end();
                    validate_registry_name(filename)?;
                    group.push(self.root.join(filename));
                }
                if group.is_empty() {
                    break;
                }
                let output = self.allocate_run_path("merge");
                let merge = merge_runs(&group, &output)?;
                self.stats.bytes_written =
                    self.stats.bytes_written.saturating_add(merge.bytes_written);
                self.stats.records_out = merge.records;
                self.stats.max_fan_in = self.stats.max_fan_in.max(group.len());
                self.stats.peak_open_files = self
                    .stats
                    .peak_open_files
                    .max(group.len().saturating_add(1));
                writeln!(
                    destination,
                    "{}",
                    output
                        .file_name()
                        .and_then(|value| value.to_str())
                        .ok_or_else(|| PocError::Integrity(
                            "semantic spool filename is not UTF-8".to_owned()
                        ))?
                )
                .map_err(|error| {
                    PocError::io("write semantic merge registry", &next_registry, error)
                })?;
                for input in group {
                    std::fs::remove_file(&input).map_err(|error| {
                        PocError::io("remove merged semantic run", &input, error)
                    })?;
                }
                output_count = output_count.saturating_add(1);
            }
            destination.flush().map_err(|error| {
                PocError::io("flush semantic merge registry", &next_registry, error)
            })?;
            destination.get_ref().sync_all().map_err(|error| {
                PocError::io("fsync semantic merge registry", &next_registry, error)
            })?;
            if registry != self.registry {
                std::fs::remove_file(&registry).map_err(|error| {
                    PocError::io("remove old semantic run registry", &registry, error)
                })?;
            }
            registry = next_registry;
            run_count = output_count;
            self.stats.merge_passes = self.stats.merge_passes.saturating_add(1);
        }

        let final_path = sole_registry_path(&registry, &self.root)?;
        self.stats.records_out = self.stats.records_in;
        Ok(SortedSpool {
            path: final_path,
            stats: self.stats,
        })
    }

    fn flush_run(&mut self) -> PocResult<()> {
        self.entries
            .sort_unstable_by(|left, right| left.key.cmp(&right.key));
        reject_duplicate_entries(&self.entries)?;
        let path = self.allocate_run_path("initial");
        let result = write_run(
            &path,
            self.entries
                .drain(..)
                .map(|entry| (entry.key, entry.payload)),
        )?;
        append_registry(&self.registry, &path)?;
        self.stats.initial_runs = self.stats.initial_runs.saturating_add(1);
        self.stats.bytes_written = self
            .stats
            .bytes_written
            .saturating_add(result.bytes_written);
        self.stats.peak_open_files = self.stats.peak_open_files.max(2);
        self.buffered_bytes = 0;
        Ok(())
    }

    fn allocate_run_path(&mut self, class: &str) -> PathBuf {
        let sequence = self.next_run;
        self.next_run = self.next_run.saturating_add(1);
        self.root.join(format!("{class}-{sequence:016x}.run"))
    }
}

#[derive(Clone, Debug)]
pub struct SortedSpool {
    path: PathBuf,
    stats: SpoolStats,
}

impl SortedSpool {
    pub const fn stats(&self) -> SpoolStats {
        self.stats
    }

    pub fn for_each(
        &self,
        mut visitor: impl FnMut(&[u8], &[u8]) -> PocResult<()>,
    ) -> PocResult<()> {
        let mut reader = RunReader::open(&self.path)?;
        let mut previous = None;
        while let Some(entry) = reader.next_entry()? {
            if previous
                .as_ref()
                .is_some_and(|value: &Vec<u8>| value >= &entry.key)
            {
                return Err(PocError::Integrity(
                    "semantic sorted run is not strictly ordered".to_owned(),
                ));
            }
            visitor(&entry.key, &entry.payload)?;
            previous = Some(entry.key);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct WriteResult {
    records: u64,
    bytes_written: u64,
}

fn write_run(
    path: &Path,
    entries: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
) -> PocResult<WriteResult> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| PocError::io("create semantic spool run", path, error))?;
    let mut writer = BufWriter::with_capacity(IO_BUFFER_BYTES, file);
    writer
        .write_all(RUN_MAGIC)
        .map_err(|error| PocError::io("write semantic spool run", path, error))?;
    let mut records = 0_u64;
    let mut bytes_written = 8_u64;
    for (key, payload) in entries {
        validate_entry(&key, &payload)?;
        let key_length = u32::try_from(key.len())
            .map_err(|_| PocError::Integrity("semantic spool key overflow".to_owned()))?;
        let payload_length = u32::try_from(payload.len())
            .map_err(|_| PocError::Integrity("semantic spool payload overflow".to_owned()))?;
        writer
            .write_all(&key_length.to_be_bytes())
            .and_then(|()| writer.write_all(&payload_length.to_be_bytes()))
            .and_then(|()| writer.write_all(&key))
            .and_then(|()| writer.write_all(&payload))
            .map_err(|error| PocError::io("write semantic spool entry", path, error))?;
        records = records.saturating_add(1);
        bytes_written = bytes_written
            .saturating_add(u64::try_from(8 + key.len() + payload.len()).unwrap_or(u64::MAX));
    }
    writer
        .flush()
        .map_err(|error| PocError::io("flush semantic spool run", path, error))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| PocError::io("fsync semantic spool run", path, error))?;
    Ok(WriteResult {
        records,
        bytes_written,
    })
}

fn merge_runs(inputs: &[PathBuf], output: &Path) -> PocResult<WriteResult> {
    if inputs.is_empty() || inputs.len() > SEMANTIC_MERGE_FAN_IN {
        return Err(PocError::Integrity(
            "semantic merge fan-in is outside fixed bounds".to_owned(),
        ));
    }
    let mut readers = inputs
        .iter()
        .map(|path| RunReader::open(path))
        .collect::<PocResult<Vec<_>>>()?;
    let mut heads = readers
        .iter_mut()
        .map(RunReader::next_entry)
        .collect::<PocResult<Vec<_>>>()?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|error| PocError::io("create merged semantic run", output, error))?;
    let mut writer = BufWriter::with_capacity(IO_BUFFER_BYTES, file);
    writer
        .write_all(RUN_MAGIC)
        .map_err(|error| PocError::io("write merged semantic run", output, error))?;
    let mut records = 0_u64;
    let mut bytes_written = 8_u64;
    let mut previous = None;
    loop {
        let next_index = heads
            .iter()
            .enumerate()
            .filter_map(|(index, value)| value.as_ref().map(|entry| (index, &entry.key)))
            .min_by(|left, right| left.1.cmp(right.1))
            .map(|(index, _)| index);
        let Some(index) = next_index else {
            break;
        };
        let entry = heads[index]
            .take()
            .ok_or_else(|| PocError::Integrity("semantic merge head disappeared".to_owned()))?;
        if previous
            .as_ref()
            .is_some_and(|value: &Vec<u8>| value >= &entry.key)
        {
            return Err(PocError::Integrity(
                "duplicate or unordered canonical semantic key".to_owned(),
            ));
        }
        let key_length = u32::try_from(entry.key.len())
            .map_err(|_| PocError::Integrity("semantic merge key overflow".to_owned()))?;
        let payload_length = u32::try_from(entry.payload.len())
            .map_err(|_| PocError::Integrity("semantic merge payload overflow".to_owned()))?;
        writer
            .write_all(&key_length.to_be_bytes())
            .and_then(|()| writer.write_all(&payload_length.to_be_bytes()))
            .and_then(|()| writer.write_all(&entry.key))
            .and_then(|()| writer.write_all(&entry.payload))
            .map_err(|error| PocError::io("write merged semantic entry", output, error))?;
        records = records.saturating_add(1);
        bytes_written = bytes_written.saturating_add(
            u64::try_from(8 + entry.key.len() + entry.payload.len()).unwrap_or(u64::MAX),
        );
        previous = Some(entry.key);
        heads[index] = readers[index].next_entry()?;
    }
    writer
        .flush()
        .map_err(|error| PocError::io("flush merged semantic run", output, error))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| PocError::io("fsync merged semantic run", output, error))?;
    Ok(WriteResult {
        records,
        bytes_written,
    })
}

struct RunReader {
    path: PathBuf,
    reader: BufReader<File>,
}

impl RunReader {
    fn open(path: &Path) -> PocResult<Self> {
        let file = File::open(path)
            .map_err(|error| PocError::io("open semantic spool run", path, error))?;
        let mut reader = BufReader::with_capacity(IO_BUFFER_BYTES, file);
        let mut magic = [0_u8; 8];
        reader
            .read_exact(&mut magic)
            .map_err(|error| PocError::io("read semantic spool magic", path, error))?;
        if &magic != RUN_MAGIC {
            return Err(PocError::Integrity(
                "semantic spool run has wrong magic".to_owned(),
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
            reader,
        })
    }

    fn next_entry(&mut self) -> PocResult<Option<Entry>> {
        let mut header = [0_u8; 8];
        if !read_exact_or_eof(&mut self.reader, &mut header)
            .map_err(|error| PocError::io("read semantic spool header", &self.path, error))?
        {
            return Ok(None);
        }
        let key_length = usize::try_from(u32::from_be_bytes(
            header[..4].try_into().expect("fixed slice"),
        ))
        .map_err(|_| PocError::Integrity("semantic spool key length overflow".to_owned()))?;
        let payload_length = usize::try_from(u32::from_be_bytes(
            header[4..].try_into().expect("fixed slice"),
        ))
        .map_err(|_| PocError::Integrity("semantic spool payload length overflow".to_owned()))?;
        if key_length == 0 || key_length > MAX_KEY_BYTES || payload_length > MAX_RECORD_BYTES {
            return Err(PocError::Integrity(
                "semantic spool entry exceeds fixed bounds".to_owned(),
            ));
        }
        let mut key = vec![0_u8; key_length];
        let mut payload = vec![0_u8; payload_length];
        self.reader
            .read_exact(&mut key)
            .and_then(|()| self.reader.read_exact(&mut payload))
            .map_err(|error| PocError::io("read semantic spool entry", &self.path, error))?;
        Ok(Some(Entry { key, payload }))
    }
}

fn read_exact_or_eof(reader: &mut impl Read, bytes: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < bytes.len() {
        let count = reader.read(&mut bytes[filled..])?;
        if count == 0 {
            if filled == 0 {
                return Ok(false);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "partial semantic spool frame",
            ));
        }
        filled += count;
    }
    Ok(true)
}

fn validate_entry(key: &[u8], payload: &[u8]) -> PocResult<()> {
    if key.is_empty()
        || key.len() > MAX_KEY_BYTES
        || payload.is_empty()
        || payload.len() > MAX_RECORD_BYTES
    {
        return Err(PocError::Integrity(
            "semantic spool entry is outside fixed bounds".to_owned(),
        ));
    }
    Ok(())
}

fn reject_duplicate_entries(entries: &[Entry]) -> PocResult<()> {
    if entries.windows(2).any(|pair| pair[0].key >= pair[1].key) {
        return Err(PocError::Integrity(
            "duplicate canonical key within semantic spool run".to_owned(),
        ));
    }
    Ok(())
}

fn append_registry(registry: &Path, run: &Path) -> PocResult<()> {
    let name = run
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PocError::Integrity("semantic run filename is not UTF-8".to_owned()))?;
    validate_registry_name(name)?;
    let mut file = OpenOptions::new()
        .append(true)
        .open(registry)
        .map_err(|error| PocError::io("open semantic run registry", registry, error))?;
    writeln!(file, "{name}")
        .and_then(|()| file.sync_all())
        .map_err(|error| PocError::io("append semantic run registry", registry, error))
}

fn validate_registry_name(name: &str) -> PocResult<()> {
    if name.is_empty()
        || name.len() > 128
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
    {
        return Err(PocError::Integrity(
            "invalid semantic spool registry entry".to_owned(),
        ));
    }
    Ok(())
}

fn sole_registry_path(registry: &Path, root: &Path) -> PocResult<PathBuf> {
    let mut reader = BufReader::new(
        File::open(registry)
            .map_err(|error| PocError::io("open final semantic run registry", registry, error))?,
    );
    let mut first = String::new();
    if reader
        .read_line(&mut first)
        .map_err(|error| PocError::io("read final semantic run registry", registry, error))?
        == 0
    {
        return Err(PocError::Integrity(
            "final semantic run registry is empty".to_owned(),
        ));
    }
    let mut extra = String::new();
    if reader
        .read_line(&mut extra)
        .map_err(|error| PocError::io("read final semantic run registry", registry, error))?
        != 0
    {
        return Err(PocError::Integrity(
            "final semantic run registry names multiple runs".to_owned(),
        ));
    }
    let name = first.trim_end();
    validate_registry_name(name)?;
    Ok(root.join(name))
}

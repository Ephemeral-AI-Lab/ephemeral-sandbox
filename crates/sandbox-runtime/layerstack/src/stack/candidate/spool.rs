use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

const MAX_MANAGED_BYTES: usize = 4 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
const MERGE_FAN_IN: usize = 8;
const RUN_READER_BYTES: usize = 64 * 1024;
const RECORD_OVERHEAD_BYTES: usize = 96;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum MutationAction {
    Remove = 1,
    Replace = 2,
    OpaqueDirectory = 3,
}

impl MutationAction {
    fn from_u8(value: u8) -> Result<Self, SpoolError> {
        match value {
            1 => Ok(Self::Remove),
            2 => Ok(Self::Replace),
            3 => Ok(Self::OpaqueDirectory),
            _ => Err(SpoolError::CorruptRun("unknown mutation action")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationRecord {
    pub(crate) path: Vec<u8>,
    pub(crate) action: MutationAction,
    pub(crate) conflict_group: Option<[u8; 16]>,
    pub(crate) descriptor: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SpoolStats {
    pub(crate) records_in: u64,
    pub(crate) records_out: u64,
    pub(crate) initial_runs: usize,
    pub(crate) merge_passes: usize,
    pub(crate) maximum_fan_in: usize,
    pub(crate) maximum_buffer_bytes: usize,
}

#[derive(Debug)]
pub(crate) enum SpoolError {
    Io(std::io::Error),
    InvalidLimit,
    InvalidPath,
    DescriptorLimit,
    CorruptRun(&'static str),
    SequenceOverflow,
}

impl fmt::Display for SpoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "changed-path spool I/O failed: {error}"),
            Self::InvalidLimit => write!(formatter, "invalid changed-path spool memory limit"),
            Self::InvalidPath => write!(formatter, "invalid changed-path spool path"),
            Self::DescriptorLimit => write!(formatter, "mutation descriptor exceeds 64 KiB"),
            Self::CorruptRun(reason) => write!(formatter, "corrupt changed-path run: {reason}"),
            Self::SequenceOverflow => write!(formatter, "changed-path sequence overflow"),
        }
    }
}

impl std::error::Error for SpoolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidLimit
            | Self::InvalidPath
            | Self::DescriptorLimit
            | Self::CorruptRun(_)
            | Self::SequenceOverflow => None,
        }
    }
}

impl From<std::io::Error> for SpoolError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
struct SequencedRecord {
    sequence: u64,
    record: MutationRecord,
}

pub(crate) struct ChangedPathSpool {
    work_dir: PathBuf,
    memory_limit: usize,
    buffered_bytes: usize,
    buffer: Vec<SequencedRecord>,
    runs: Vec<PathBuf>,
    next_sequence: u64,
    next_run: u64,
    stats: SpoolStats,
    disarmed: bool,
}

impl ChangedPathSpool {
    pub(crate) fn new(work_dir: PathBuf, memory_limit: usize) -> Result<Self, SpoolError> {
        if !(1..=MAX_MANAGED_BYTES).contains(&memory_limit) {
            return Err(SpoolError::InvalidLimit);
        }
        std::fs::create_dir(&work_dir)?;
        Ok(Self {
            work_dir,
            memory_limit,
            buffered_bytes: 0,
            buffer: Vec::new(),
            runs: Vec::new(),
            next_sequence: 0,
            next_run: 0,
            stats: SpoolStats::default(),
            disarmed: false,
        })
    }

    pub(crate) fn push(&mut self, record: MutationRecord) -> Result<(), SpoolError> {
        validate_record(&record)?;
        let cost = record_cost(&record)?;
        if !self.buffer.is_empty()
            && self
                .buffered_bytes
                .checked_add(cost)
                .is_none_or(|total| total > self.memory_limit)
        {
            self.spill()?;
        }
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(SpoolError::SequenceOverflow)?;
        self.buffer.push(SequencedRecord { sequence, record });
        self.buffered_bytes = self.buffered_bytes.saturating_add(cost);
        self.stats.records_in += 1;
        self.stats.maximum_buffer_bytes = self.stats.maximum_buffer_bytes.max(self.buffered_bytes);
        if self.buffered_bytes >= self.memory_limit {
            self.spill()?;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<SortedSpool, SpoolError> {
        if !self.buffer.is_empty() {
            self.spill()?;
        }
        self.stats.initial_runs = self.runs.len();
        if self.runs.is_empty() {
            let path = self.next_run_path("empty");
            durable_empty_file(&path)?;
            self.runs.push(path);
        }

        while self.runs.len() > 1 {
            self.stats.merge_passes += 1;
            let inputs = std::mem::take(&mut self.runs);
            let mut outputs = Vec::with_capacity(inputs.len().div_ceil(MERGE_FAN_IN));
            for group in inputs.chunks(MERGE_FAN_IN) {
                self.stats.maximum_fan_in = self.stats.maximum_fan_in.max(group.len());
                let output = self.next_run_path("merge");
                merge_runs(group, &output)?;
                outputs.push(output);
            }
            for input in inputs {
                std::fs::remove_file(input)?;
            }
            self.runs = outputs;
        }

        let path = self.runs.pop().expect("one final changed-path run");
        let mut records_out = 0_u64;
        read_run(&path, |_| {
            records_out += 1;
            Ok(())
        })?;
        self.stats.records_out = records_out;
        self.disarmed = true;
        Ok(SortedSpool {
            work_dir: self.work_dir.clone(),
            path,
            stats: self.stats,
        })
    }

    fn spill(&mut self) -> Result<(), SpoolError> {
        self.buffer.sort_unstable_by(|left, right| {
            left.record
                .path
                .cmp(&right.record.path)
                .then(left.sequence.cmp(&right.sequence))
        });
        let path = self.next_run_path("run");
        let mut writer = RunWriter::new(&path)?;
        let mut winner: Option<SequencedRecord> = None;
        for record in self.buffer.drain(..) {
            if winner
                .as_ref()
                .is_some_and(|current| current.record.path != record.record.path)
            {
                writer.write(winner.take().expect("changed-path winner"))?;
            }
            winner = Some(record);
        }
        if let Some(record) = winner {
            writer.write(record)?;
        }
        writer.finish()?;
        self.buffered_bytes = 0;
        self.runs.push(path);
        Ok(())
    }

    fn next_run_path(&mut self, label: &str) -> PathBuf {
        let number = self.next_run;
        self.next_run += 1;
        self.work_dir.join(format!("{label}-{number:016x}.bin"))
    }
}

impl Drop for ChangedPathSpool {
    fn drop(&mut self) {
        if !self.disarmed {
            let _ = std::fs::remove_dir_all(&self.work_dir);
        }
    }
}

pub(crate) struct SortedSpool {
    work_dir: PathBuf,
    path: PathBuf,
    stats: SpoolStats,
}

impl SortedSpool {
    pub(crate) const fn stats(&self) -> SpoolStats {
        self.stats
    }

    pub(crate) fn for_each(
        &self,
        mut visit: impl FnMut(MutationRecord) -> Result<(), SpoolError>,
    ) -> Result<(), SpoolError> {
        read_run(&self.path, |record| visit(record.record))
    }
}

impl Drop for SortedSpool {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

fn validate_record(record: &MutationRecord) -> Result<(), SpoolError> {
    if record.path.is_empty()
        || record.path.len() > MAX_PATH_BYTES
        || record.path.contains(&0)
        || record.path.split(|byte| *byte == b'/').any(|component| {
            component.is_empty() || component.len() > 255 || matches!(component, b"." | b"..")
        })
        || record.path.split(|byte| *byte == b'/').count() > 64
    {
        return Err(SpoolError::InvalidPath);
    }
    if record.descriptor.len() > MAX_DESCRIPTOR_BYTES {
        return Err(SpoolError::DescriptorLimit);
    }
    Ok(())
}

fn record_cost(record: &MutationRecord) -> Result<usize, SpoolError> {
    RECORD_OVERHEAD_BYTES
        .checked_add(record.path.len())
        .and_then(|value| value.checked_add(record.descriptor.len()))
        .ok_or(SpoolError::InvalidLimit)
}

struct RunWriter {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl RunWriter {
    fn new(path: &Path) -> Result<Self, SpoolError> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            writer: BufWriter::new(file),
        })
    }

    fn write(&mut self, record: SequencedRecord) -> Result<(), SpoolError> {
        let path_len =
            u16::try_from(record.record.path.len()).map_err(|_| SpoolError::InvalidPath)?;
        let descriptor_len = u32::try_from(record.record.descriptor.len())
            .map_err(|_| SpoolError::DescriptorLimit)?;
        self.writer.write_all(&path_len.to_be_bytes())?;
        self.writer.write_all(&[record.record.action as u8])?;
        self.writer
            .write_all(&[u8::from(record.record.conflict_group.is_some())])?;
        self.writer.write_all(&descriptor_len.to_be_bytes())?;
        self.writer.write_all(&record.sequence.to_be_bytes())?;
        self.writer.write_all(&record.record.path)?;
        if let Some(group) = record.record.conflict_group {
            self.writer.write_all(&group)?;
        }
        self.writer.write_all(&record.record.descriptor)?;
        Ok(())
    }

    fn finish(mut self) -> Result<(), SpoolError> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        drop(self.writer);
        fsync_dir(self.path.parent().ok_or(SpoolError::InvalidPath)?)?;
        Ok(())
    }
}

struct RunReader {
    reader: BufReader<File>,
}

impl RunReader {
    fn new(path: &Path) -> Result<Self, SpoolError> {
        Ok(Self {
            reader: BufReader::with_capacity(RUN_READER_BYTES, File::open(path)?),
        })
    }

    fn next(&mut self) -> Result<Option<SequencedRecord>, SpoolError> {
        let mut first = [0_u8; 1];
        if self.reader.read(&mut first)? == 0 {
            return Ok(None);
        }
        let mut rest = [0_u8; 15];
        self.reader.read_exact(&mut rest)?;
        let path_len = usize::from(u16::from_be_bytes([first[0], rest[0]]));
        let action = MutationAction::from_u8(rest[1])?;
        let group_present = match rest[2] {
            0 => false,
            1 => true,
            _ => return Err(SpoolError::CorruptRun("invalid conflict-group flag")),
        };
        let descriptor_len = usize::try_from(u32::from_be_bytes(
            rest[3..7].try_into().expect("four bytes"),
        ))
        .map_err(|_| SpoolError::CorruptRun("descriptor length overflow"))?;
        let sequence = u64::from_be_bytes(rest[7..15].try_into().expect("eight bytes"));
        if path_len == 0 || path_len > MAX_PATH_BYTES || descriptor_len > MAX_DESCRIPTOR_BYTES {
            return Err(SpoolError::CorruptRun("record length exceeds bounds"));
        }
        let mut path = vec![0_u8; path_len];
        self.reader.read_exact(&mut path)?;
        let conflict_group = if group_present {
            let mut group = [0_u8; 16];
            self.reader.read_exact(&mut group)?;
            Some(group)
        } else {
            None
        };
        let mut descriptor = vec![0_u8; descriptor_len];
        self.reader.read_exact(&mut descriptor)?;
        let record = MutationRecord {
            path,
            action,
            conflict_group,
            descriptor,
        };
        validate_record(&record).map_err(|_| SpoolError::CorruptRun("invalid record"))?;
        Ok(Some(SequencedRecord { sequence, record }))
    }
}

#[derive(Debug)]
struct HeapItem {
    run: usize,
    record: SequencedRecord,
}

impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.run == other.run
            && self.record.sequence == other.record.sequence
            && self.record.record.path == other.record.record.path
    }
}

impl Eq for HeapItem {}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .record
            .record
            .path
            .cmp(&self.record.record.path)
            .then(other.record.sequence.cmp(&self.record.sequence))
            .then(other.run.cmp(&self.run))
    }
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn merge_runs(inputs: &[PathBuf], output: &Path) -> Result<(), SpoolError> {
    let mut readers = inputs
        .iter()
        .map(|path| RunReader::new(path))
        .collect::<Result<Vec<_>, _>>()?;
    let mut heap = BinaryHeap::new();
    for (run, reader) in readers.iter_mut().enumerate() {
        if let Some(record) = reader.next()? {
            heap.push(HeapItem { run, record });
        }
    }
    let mut writer = RunWriter::new(output)?;
    while let Some(item) = heap.pop() {
        let run = item.run;
        let path = item.record.record.path.clone();
        let mut winner = item.record;
        advance_reader(run, &mut readers, &mut heap)?;
        while heap
            .peek()
            .is_some_and(|next| next.record.record.path == path)
        {
            let next = heap.pop().expect("peeked changed-path item");
            let next_run = next.run;
            if next.record.sequence > winner.sequence {
                winner = next.record;
            }
            advance_reader(next_run, &mut readers, &mut heap)?;
        }
        writer.write(winner)?;
    }
    writer.finish()
}

fn advance_reader(
    run: usize,
    readers: &mut [RunReader],
    heap: &mut BinaryHeap<HeapItem>,
) -> Result<(), SpoolError> {
    if let Some(record) = readers[run].next()? {
        heap.push(HeapItem { run, record });
    }
    Ok(())
}

fn read_run(
    path: &Path,
    mut visit: impl FnMut(SequencedRecord) -> Result<(), SpoolError>,
) -> Result<(), SpoolError> {
    let mut reader = RunReader::new(path)?;
    while let Some(record) = reader.next()? {
        visit(record)?;
    }
    Ok(())
}

fn durable_empty_file(path: &Path) -> Result<(), SpoolError> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.sync_all()?;
    fsync_dir(path.parent().ok_or(SpoolError::InvalidPath)?)
}

#[cfg(not(windows))]
fn fsync_dir(path: &Path) -> Result<(), SpoolError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn fsync_dir(_path: &Path) -> Result<(), SpoolError> {
    Ok(())
}

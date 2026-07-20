use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::{SandboxId, SandboxResourceMetrics, SandboxStore};

pub const RESOURCE_RING_BYTES: u64 = 64 * 1024;
pub const RESOURCE_RECORD_BYTES: usize = 64;
pub const MAX_RESOURCE_RESPONSE_RECORDS: usize = 512;
const HEADER_BYTES: usize = 64;
const FORMAT_VERSION: u32 = 1;
const MAGIC: [u8; 8] = *b"EOSRING\0";
const CAPACITY: u32 =
    ((RESOURCE_RING_BYTES as usize - HEADER_BYTES) / RESOURCE_RECORD_BYTES) as u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceSample {
    pub sampled_at_unix_ms: i64,
    pub metrics: SandboxResourceMetrics,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResourceRingRead {
    pub samples: Vec<ResourceSample>,
    pub error: Option<String>,
}

pub struct ResourceRingStore {
    root: PathBuf,
    operation_state: Mutex<ResourceRingState>,
}

#[derive(Default)]
struct ResourceRingState {
    latest_samples: HashMap<String, ResourceSample>,
}

impl ResourceRingStore {
    #[must_use]
    pub fn for_store(store: &SandboxStore) -> Self {
        let root = store
            .registry_path()
            .map_or_else(default_ring_root, |registry| {
                registry
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."))
                    .join("observability-resources")
            });
        Self::new(root)
    }

    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            operation_state: Mutex::new(ResourceRingState::default()),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn path(&self, id: &SandboxId) -> PathBuf {
        self.root.join(format!("{}.ring", id.as_str()))
    }

    pub fn append(&self, id: &SandboxId, sample: ResourceSample) -> std::io::Result<()> {
        let mut state = self.guard();
        self.append_locked(id, sample)?;
        state.latest_samples.insert(id.as_str().to_owned(), sample);
        Ok(())
    }

    pub(crate) fn append_if(
        &self,
        id: &SandboxId,
        sample: ResourceSample,
        eligible: impl FnOnce() -> bool,
    ) -> std::io::Result<bool> {
        let mut state = self.guard();
        if !eligible() {
            return Ok(false);
        }
        self.append_locked(id, sample)?;
        state.latest_samples.insert(id.as_str().to_owned(), sample);
        Ok(true)
    }

    #[must_use]
    pub fn read_window(&self, id: &SandboxId, window_ms: i64) -> ResourceRingRead {
        let mut state = self.guard();
        let read = match self.read_locked(id, window_ms) {
            Ok(read) => read,
            Err(error) => ResourceRingRead {
                samples: Vec::new(),
                error: Some(error.to_string()),
            },
        };
        if read.error.is_none() {
            if let Some(sample) = read.samples.last().copied() {
                state.latest_samples.insert(id.as_str().to_owned(), sample);
            }
        } else {
            state.latest_samples.remove(id.as_str());
        }
        read
    }

    /// Read only the current committed sample. Fleet current-usage reads use
    /// this path so their I/O stays constant per sandbox instead of scanning a
    /// full 64-KiB history ring on every cadence.
    #[must_use]
    pub fn read_latest(&self, id: &SandboxId) -> ResourceRingRead {
        let mut state = self.guard();
        if let Some(sample) = state.latest_samples.get(id.as_str()).copied() {
            return ResourceRingRead {
                samples: vec![sample],
                error: None,
            };
        }
        let read = match self.read_latest_locked(id) {
            Ok(read) => read,
            Err(error) => ResourceRingRead {
                samples: Vec::new(),
                error: Some(error.to_string()),
            },
        };
        if read.error.is_none() {
            if let Some(sample) = read.samples.last().copied() {
                state.latest_samples.insert(id.as_str().to_owned(), sample);
            }
        } else {
            state.latest_samples.remove(id.as_str());
        }
        read
    }

    pub fn remove(&self, id: &SandboxId) -> std::io::Result<()> {
        let mut state = self.guard();
        let removed = match fs::remove_file(self.path(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
        if removed.is_ok() {
            state.latest_samples.remove(id.as_str());
        }
        removed
    }

    fn append_locked(&self, id: &SandboxId, sample: ResourceSample) -> std::io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let path = self.path(id);
        let (file, mut header) = open_or_recreate(&path)?;
        let record = encode_record(sample);
        let offset = HEADER_BYTES as u64 + u64::from(header.next) * RESOURCE_RECORD_BYTES as u64;
        write_all_at(&file, &record, offset)?;
        header.next = (header.next + 1) % CAPACITY;
        header.count = header.count.saturating_add(1).min(CAPACITY);
        header.sequence = header.sequence.saturating_add(1);
        write_all_at(&file, &encode_header(header), 0)
    }

    fn read_locked(&self, id: &SandboxId, window_ms: i64) -> std::io::Result<ResourceRingRead> {
        let path = self.path(id);
        let file = match OpenOptions::new().read(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ResourceRingRead {
                    samples: Vec::new(),
                    error: Some("resource ring is not available yet".to_owned()),
                });
            }
            Err(error) => return Err(error),
        };
        let header = match read_header(&file) {
            Ok(header) => header,
            Err(error) => {
                drop(file);
                recreate(&path)?;
                return Ok(ResourceRingRead {
                    samples: Vec::new(),
                    error: Some(format!(
                        "resource ring was corrupt and was recreated: {error}"
                    )),
                });
            }
        };
        let start = if header.count == CAPACITY {
            header.next
        } else {
            0
        };
        let mut samples = Vec::with_capacity(
            usize::try_from(header.count)
                .unwrap_or(MAX_RESOURCE_RESPONSE_RECORDS)
                .min(MAX_RESOURCE_RESPONSE_RECORDS),
        );
        let mut corrupt = None;
        for logical in 0..header.count {
            let index = (start + logical) % CAPACITY;
            let mut record = [0_u8; RESOURCE_RECORD_BYTES];
            let offset = HEADER_BYTES as u64 + u64::from(index) * RESOURCE_RECORD_BYTES as u64;
            if let Err(error) = read_exact_at(&file, &mut record, offset) {
                corrupt = Some(error.to_string());
                break;
            }
            match decode_record(&record) {
                Some(sample) => samples.push(sample),
                None => corrupt = Some(format!("record {index} checksum mismatch")),
            }
        }
        drop(file);
        if header.count == CAPACITY
            && samples
                .first()
                .zip(samples.get(1))
                .is_some_and(|(first, second)| first.sampled_at_unix_ms > second.sampled_at_unix_ms)
        {
            samples.remove(0);
            corrupt.get_or_insert_with(|| {
                "uncommitted newest record was discarded after header tear".to_owned()
            });
        } else if samples
            .windows(2)
            .any(|pair| pair[0].sampled_at_unix_ms > pair[1].sampled_at_unix_ms)
        {
            corrupt.get_or_insert_with(|| "record timestamps are out of order".to_owned());
        }
        if corrupt.is_some() {
            recreate(&path)?;
        }
        let newest = samples
            .last()
            .map_or(i64::MIN, |sample| sample.sampled_at_unix_ms);
        let retain_after = newest.saturating_sub(window_ms);
        samples.retain(|sample| sample.sampled_at_unix_ms >= retain_after);
        if samples.len() > MAX_RESOURCE_RESPONSE_RECORDS {
            samples.drain(..samples.len() - MAX_RESOURCE_RESPONSE_RECORDS);
        }
        Ok(ResourceRingRead {
            samples,
            error: corrupt.map(|error| {
                format!("resource ring contained a corrupt record and was recreated: {error}")
            }),
        })
    }

    fn read_latest_locked(&self, id: &SandboxId) -> std::io::Result<ResourceRingRead> {
        let path = self.path(id);
        let file = match OpenOptions::new().read(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ResourceRingRead {
                    samples: Vec::new(),
                    error: Some("resource ring is not available yet".to_owned()),
                });
            }
            Err(error) => return Err(error),
        };
        let header = match read_header(&file) {
            Ok(header) => header,
            Err(error) => {
                drop(file);
                recreate(&path)?;
                return Ok(ResourceRingRead {
                    samples: Vec::new(),
                    error: Some(format!(
                        "resource ring was corrupt and was recreated: {error}"
                    )),
                });
            }
        };
        if header.count == 0 {
            return Ok(ResourceRingRead::default());
        }

        let index = (header.next + CAPACITY - 1) % CAPACITY;
        let mut record = [0_u8; RESOURCE_RECORD_BYTES];
        let offset = HEADER_BYTES as u64 + u64::from(index) * RESOURCE_RECORD_BYTES as u64;
        read_exact_at(&file, &mut record, offset)?;
        let Some(sample) = decode_record(&record) else {
            drop(file);
            recreate(&path)?;
            return Ok(ResourceRingRead {
                samples: Vec::new(),
                error: Some(format!(
                    "resource ring contained a corrupt current record and was recreated: record {index} checksum mismatch"
                )),
            });
        };
        Ok(ResourceRingRead {
            samples: vec![sample],
            error: None,
        })
    }

    fn guard(&self) -> MutexGuard<'_, ResourceRingState> {
        self.operation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Clone, Copy)]
struct Header {
    next: u32,
    count: u32,
    sequence: u64,
}

fn open_or_recreate(path: &Path) -> std::io::Result<(File, Header)> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match read_header(&file) {
        Ok(header) => Ok((file, header)),
        Err(_) => {
            drop(file);
            let file = recreate(path)?;
            Ok((
                file,
                Header {
                    next: 0,
                    count: 0,
                    sequence: 0,
                },
            ))
        }
    }
}

fn recreate(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.set_len(RESOURCE_RING_BYTES)?;
    write_all_at(
        &file,
        &encode_header(Header {
            next: 0,
            count: 0,
            sequence: 0,
        }),
        0,
    )?;
    file.sync_data()?;
    Ok(file)
}

fn read_header(file: &File) -> std::io::Result<Header> {
    if file.metadata()?.len() != RESOURCE_RING_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "resource ring has the wrong length",
        ));
    }
    let mut bytes = [0_u8; HEADER_BYTES];
    read_exact_at(file, &mut bytes, 0)?;
    if bytes[..8] != MAGIC
        || u32_at(&bytes, 8) != FORMAT_VERSION
        || u32_at(&bytes, 12) as usize != HEADER_BYTES
        || u32_at(&bytes, 16) as usize != RESOURCE_RECORD_BYTES
        || u32_at(&bytes, 20) != CAPACITY
        || u64_at(&bytes, 40) != checksum(&bytes[..40])
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "resource ring header is invalid",
        ));
    }
    let header = Header {
        next: u32_at(&bytes, 24),
        count: u32_at(&bytes, 28),
        sequence: u64_at(&bytes, 32),
    };
    if header.next >= CAPACITY || header.count > CAPACITY {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "resource ring header indices are invalid",
        ));
    }
    Ok(header)
}

fn encode_header(header: Header) -> [u8; HEADER_BYTES] {
    let mut bytes = [0_u8; HEADER_BYTES];
    bytes[..8].copy_from_slice(&MAGIC);
    put_u32(&mut bytes, 8, FORMAT_VERSION);
    put_u32(&mut bytes, 12, HEADER_BYTES as u32);
    put_u32(&mut bytes, 16, RESOURCE_RECORD_BYTES as u32);
    put_u32(&mut bytes, 20, CAPACITY);
    put_u32(&mut bytes, 24, header.next);
    put_u32(&mut bytes, 28, header.count);
    put_u64(&mut bytes, 32, header.sequence);
    let value = checksum(&bytes[..40]);
    put_u64(&mut bytes, 40, value);
    bytes
}

fn encode_record(sample: ResourceSample) -> [u8; RESOURCE_RECORD_BYTES] {
    let mut bytes = [0_u8; RESOURCE_RECORD_BYTES];
    put_u64(&mut bytes, 0, sample.sampled_at_unix_ms as u64);
    let values = [
        sample.metrics.cpu_usage_usec,
        sample.metrics.memory_current_bytes,
        sample.metrics.memory_limit_bytes,
        sample.metrics.io_read_bytes,
        sample.metrics.io_write_bytes,
    ];
    let mut validity = 0_u64;
    for (index, value) in values.into_iter().enumerate() {
        if let Some(value) = value {
            validity |= 1 << index;
            put_u64(&mut bytes, 16 + index * 8, value);
        }
    }
    put_u64(&mut bytes, 8, validity);
    let value = checksum(&bytes[..56]);
    put_u64(&mut bytes, 56, value);
    bytes
}

fn decode_record(bytes: &[u8; RESOURCE_RECORD_BYTES]) -> Option<ResourceSample> {
    if u64_at(bytes, 56) != checksum(&bytes[..56]) {
        return None;
    }
    let validity = u64_at(bytes, 8);
    let value =
        |index: usize| (validity & (1 << index) != 0).then(|| u64_at(bytes, 16 + index * 8));
    Some(ResourceSample {
        sampled_at_unix_ms: u64_at(bytes, 0) as i64,
        metrics: SandboxResourceMetrics {
            cpu_usage_usec: value(0),
            memory_current_bytes: value(1),
            memory_limit_bytes: value(2),
            io_read_bytes: value(3),
            io_write_bytes: value(4),
        },
    })
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or_default())
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap_or_default())
}

#[cfg(unix)]
fn write_all_at(file: &File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;

    file.write_all_at(bytes, offset)
}

#[cfg(unix)]
fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;

    file.read_exact_at(bytes, offset)
}

#[cfg(not(unix))]
fn write_all_at(file: &File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};

    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(bytes)
}

fn default_ring_root() -> PathBuf {
    if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(root)
            .join("eos-sandbox")
            .join("observability-resources");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    if let Some(home) = home.as_ref() {
        return home
            .join("Library/Application Support/eos-sandbox")
            .join("observability-resources");
    }
    home.unwrap_or_else(std::env::temp_dir)
        .join(".local/state/eos-sandbox")
        .join("observability-resources")
}

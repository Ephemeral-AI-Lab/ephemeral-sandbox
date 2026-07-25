use std::fmt;
use std::io::{self, Read};

pub(crate) const MIN_CHUNK_BYTES: usize = 8 * 1024;
pub(crate) const TARGET_CHUNK_BYTES: usize = 16 * 1024;
pub(crate) const MAX_CHUNK_BYTES: usize = 32 * 1024;

const SEQUENCE_THRESHOLD: usize = 5;
const OPPOSING_TRIGGER: usize = 50;
const JUMP_BYTES: usize = 512;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ChunkSlices<'a> {
    first: &'a [u8],
    second: &'a [u8],
}

impl<'a> ChunkSlices<'a> {
    const fn new(first: &'a [u8], second: &'a [u8]) -> Self {
        Self { first, second }
    }

    pub(crate) const fn first(self) -> &'a [u8] {
        self.first
    }

    pub(crate) const fn second(self) -> &'a [u8] {
        self.second
    }

    pub(crate) const fn len(self) -> usize {
        self.first.len() + self.second.len()
    }

    pub(crate) fn is_all_zero(self) -> bool {
        self.first.iter().chain(self.second).all(|byte| *byte == 0)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct StreamStats {
    pub(crate) input_bytes: u64,
    pub(crate) chunks: u64,
    pub(crate) read_calls: u64,
    pub(crate) interrupted_reads: u64,
    pub(crate) max_buffered: usize,
    pub(crate) max_slices: usize,
}

#[derive(Debug)]
pub(crate) enum StreamError<E> {
    Io(io::Error),
    Sink(E),
}

impl<E: fmt::Display> fmt::Display for StreamError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "SeqCDC input failed: {error}"),
            Self::Sink(error) => write!(formatter, "SeqCDC sink failed: {error}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for StreamError<E> {}

pub(crate) fn stream<R, F, E>(reader: &mut R, mut sink: F) -> Result<StreamStats, StreamError<E>>
where
    R: Read,
    F: FnMut(ChunkSlices<'_>) -> Result<(), E>,
{
    let mut ring = [0_u8; MAX_CHUNK_BYTES];
    let mut head = 0_usize;
    let mut buffered = 0_usize;
    let mut eof = false;
    let mut stats = StreamStats::default();

    loop {
        while !eof && buffered < MAX_CHUNK_BYTES {
            let tail = (head + buffered) % MAX_CHUNK_BYTES;
            let free = MAX_CHUNK_BYTES - buffered;
            let contiguous = if tail < head {
                head - tail
            } else {
                (MAX_CHUNK_BYTES - tail).min(free)
            };
            stats.read_calls += 1;
            match reader.read(&mut ring[tail..tail + contiguous]) {
                Ok(0) => eof = true,
                Ok(count) => {
                    buffered += count;
                    stats.input_bytes += u64::try_from(count).unwrap_or(u64::MAX);
                    stats.max_buffered = stats.max_buffered.max(buffered);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    stats.interrupted_reads += 1;
                }
                Err(error) => return Err(StreamError::Io(error)),
            }
        }

        if buffered == 0 {
            return Ok(stats);
        }

        let cut = if eof && buffered <= MIN_CHUNK_BYTES {
            buffered
        } else {
            find_cut(buffered, |offset| ring[(head + offset) % MAX_CHUNK_BYTES])
        };
        debug_assert!((1..=buffered).contains(&cut));

        let first_len = cut.min(MAX_CHUNK_BYTES - head);
        let chunk = ChunkSlices::new(&ring[head..head + first_len], &ring[..cut - first_len]);
        stats.max_slices = stats
            .max_slices
            .max(usize::from(!chunk.first.is_empty()) + usize::from(!chunk.second.is_empty()));
        sink(chunk).map_err(StreamError::Sink)?;
        stats.chunks += 1;
        head = (head + cut) % MAX_CHUNK_BYTES;
        buffered -= cut;
    }
}

fn find_cut(mut available: usize, mut byte_at: impl FnMut(usize) -> u8) -> usize {
    available = available.min(MAX_CHUNK_BYTES);
    let mut position = MIN_CHUNK_BYTES;
    let mut opposing = 0_usize;
    let mut sequence = 0_usize;

    while position < available {
        let current = byte_at(position);
        let previous = byte_at(position - 1);
        position += 1;
        if current == previous {
            continue;
        }

        let is_opposing = current < previous;
        opposing += usize::from(is_opposing);
        sequence = if is_opposing { 0 } else { sequence + 1 };
        if sequence == SEQUENCE_THRESHOLD {
            return position - 1;
        }
        if opposing == OPPOSING_TRIGGER {
            position = position.saturating_add(JUMP_BYTES).min(available);
            opposing = 0;
        }
    }
    available
}

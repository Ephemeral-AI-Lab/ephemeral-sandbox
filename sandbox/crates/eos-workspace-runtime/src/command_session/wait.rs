use std::thread;
use std::time::{Duration, Instant};

use crate::command_session::CommandSessionConfig;

pub trait CommandSessionWaitTarget<T> {
    fn try_finalize(&self) -> Option<T>;
    fn transcript_len(&self) -> u64;
    fn read_output_since(&self, start_offset: u64) -> String;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome<T> {
    Completed(T),
    Running(String),
}

pub fn wait_for_yield<T, S>(
    session: &S,
    config: &CommandSessionConfig,
    yield_time_ms: u64,
    start_offset: u64,
) -> WaitOutcome<T>
where
    S: CommandSessionWaitTarget<T> + ?Sized,
{
    let deadline = Instant::now() + Duration::from_millis(yield_time_ms);
    let (mut last_off, mut last_change) = (start_offset, Instant::now());
    loop {
        if let Some(result) = session.try_finalize() {
            return WaitOutcome::Completed(result);
        }
        let off = session.transcript_len();
        if off != last_off {
            last_off = off;
            last_change = Instant::now();
        }
        if off > start_offset && last_change.elapsed() >= Duration::from_millis(config.quiet_ms) {
            return WaitOutcome::Running(session.read_output_since(start_offset));
        }
        if Instant::now() >= deadline {
            return WaitOutcome::Running(session.read_output_since(start_offset));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
#[path = "../../tests/command_session/wait_unit.rs"]
mod tests;

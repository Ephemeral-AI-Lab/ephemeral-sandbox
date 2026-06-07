use std::thread;
use std::time::{Duration, Instant};

use crate::CommandSessionConfig;

pub(crate) trait CommandSessionWaitTarget<T> {
    fn try_finalize(&self) -> Option<T>;
    fn transcript_len(&self) -> u64;
    fn read_output_since(&self, start_offset: u64) -> String;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WaitOutcome<T> {
    Completed(T),
    Running(String),
}

pub(crate) fn wait_for_yield<T, S>(
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
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeWaitTarget {
        output: Mutex<String>,
        offsets: Mutex<Vec<u64>>,
    }

    impl CommandSessionWaitTarget<&'static str> for FakeWaitTarget {
        fn try_finalize(&self) -> Option<&'static str> {
            None
        }

        fn transcript_len(&self) -> u64 {
            self.offsets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop()
                .unwrap_or(1)
        }

        fn read_output_since(&self, _start_offset: u64) -> String {
            self.output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    #[test]
    fn wait_returns_running_after_quiet_output() {
        let target = FakeWaitTarget {
            output: Mutex::new("ready\n".to_owned()),
            offsets: Mutex::new(vec![1, 1, 0]),
        };
        let config = CommandSessionConfig {
            quiet_ms: 1,
            ..CommandSessionConfig::default()
        };

        let result = wait_for_yield(&target, &config, 100, 0);

        assert_eq!(result, WaitOutcome::Running("ready\n".to_owned()));
    }
}

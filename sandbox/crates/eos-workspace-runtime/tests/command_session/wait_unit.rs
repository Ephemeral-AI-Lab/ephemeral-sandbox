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

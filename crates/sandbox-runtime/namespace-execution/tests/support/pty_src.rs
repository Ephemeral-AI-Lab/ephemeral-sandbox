#[allow(
    dead_code,
    reason = "the focused PTY test does not include the engine snapshot consumer"
)]
pub mod pty {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/pty.rs"));

    pub fn terminate_pgid_for_test() -> fn(i32) {
        terminate_pgid
    }

    #[test]
    fn output_reactor_drains_output_that_arrives_after_registration() {
        use std::sync::mpsc;

        let reactor = OutputReactor::new();
        let (master, mut slave) = open_pty_pair().expect("open test PTY");
        set_nonblocking(&master).expect("make test PTY nonblocking");
        let (output_tx, output_rx) = mpsc::channel();
        let drain = OutputDrain::pending();
        let terminal_drain = drain.clone();
        let activity = Arc::new(OutputActivity::default());
        let observed = activity.snapshot();
        reactor.register(
            master,
            Box::new(move |bytes| {
                let _ = output_tx.send(bytes.to_vec());
            }),
            drain,
            Arc::clone(&activity),
        );

        thread::sleep(Duration::from_millis(20));
        slave
            .write_all(b"ready\n")
            .expect("write delayed PTY output");

        let output = output_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("readiness reactor did not drain delayed PTY output");
        assert_ne!(
            activity.wait_for_change(observed, Duration::from_millis(100)),
            observed,
            "output activity was not published after sink delivery"
        );
        assert!(
            output.windows(b"ready".len()).any(|part| part == b"ready"),
            "unexpected PTY output: {output:?}"
        );

        drop(slave);
        assert!(
            terminal_drain.wait_timeout(Duration::from_millis(100)),
            "readiness reactor did not observe PTY EOF"
        );
    }
}

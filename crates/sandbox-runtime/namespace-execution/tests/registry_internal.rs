#![allow(dead_code)]

#[path = "../src/error.rs"]
mod error;
#[path = "../src/shell.rs"]
mod shell;
#[path = "../src/types.rs"]
mod types;

mod registry {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/registry.rs"));

    fn id(number: usize) -> NamespaceExecutionId {
        NamespaceExecutionId(format!("namespace_execution_{number}"))
    }

    #[test]
    fn bulk_terminal_release_compacts_backing_storage() {
        let registry = ExecutionRegistry::new(64, 64);
        for number in 0..32 {
            let execution_id = id(number);
            registry.try_reserve(&execution_id).expect("reserve slot");
            registry.attach(&execution_id, "workspace-a".to_owned());
            registry.complete(&execution_id, NamespaceExecutionTerminalStatus::Ok, Some(0));
        }
        let (entries_high_water, order_high_water) = {
            let state = registry.lock();
            (state.entries.capacity(), state.terminal_order.capacity())
        };
        assert!(entries_high_water >= 32);
        assert!(order_high_water >= 32);

        assert_eq!(
            registry.remove_terminal_values(|value| value == "workspace-a"),
            32
        );

        let state = registry.lock();
        assert!(state.entries.is_empty());
        assert!(state.terminal_order.is_empty());
        assert!(state.entries.capacity() < entries_high_water);
        assert!(state.terminal_order.capacity() < order_high_water);
    }
}

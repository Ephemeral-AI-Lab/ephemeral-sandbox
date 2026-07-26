#[global_allocator]
static GLOBAL_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

include!(env!("SANDBOX_DAEMON_ALLOCATOR_METRICS"));

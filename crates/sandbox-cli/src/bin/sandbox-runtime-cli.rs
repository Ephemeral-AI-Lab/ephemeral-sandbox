use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    sandbox_cli::runtime::run_cli(std::env::args_os()).await
}

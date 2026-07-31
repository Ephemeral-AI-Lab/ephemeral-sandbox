use std::sync::Arc;

use tokio::io::AsyncWrite;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use super::{
    connection::write_framed_response_with_timeout, error, GatewayError, SandboxGatewayServer,
};

impl SandboxGatewayServer {
    pub async fn serve(self) -> Result<(), GatewayError> {
        let server = Arc::new(self);
        prepare_paths(&server).await?;
        let listener = TcpListener::bind(server.config.bind_addr.as_str()).await?;
        tokio::fs::write(&server.config.pid_path, std::process::id().to_string()).await?;

        let permits = Arc::new(Semaphore::new(server.config.max_concurrent_connections));
        let result = accept_until_shutdown(Arc::clone(&server), listener, permits).await;
        cleanup_paths(&server).await;
        result
    }
}

async fn prepare_paths(server: &SandboxGatewayServer) -> Result<(), GatewayError> {
    if let Some(parent) = server.config.pid_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    Ok(())
}

async fn accept_until_shutdown(
    server: Arc<SandboxGatewayServer>,
    listener: TcpListener,
    permits: Arc<Semaphore>,
) -> Result<(), GatewayError> {
    loop {
        tokio::select! {
            () = server.shutdown.cancelled() => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    tokio::spawn(reject_overloaded_connection(
                        stream,
                        server.config.max_concurrent_connections,
                    ));
                    continue;
                };
                let server = Arc::clone(&server);
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = server.handle_connection(stream).await;
                });
            }
        }
    }
}

async fn reject_overloaded_connection<S>(mut stream: S, max_connections: usize)
where
    S: AsyncWrite + Unpin,
{
    let response = error::error_response(
        sandbox_operation_contract::error::INTERNAL_ERROR,
        "gateway is at connection capacity",
        serde_json::json!({ "max_concurrent_connections": max_connections }),
    );
    let framed = sandbox_protocol::response_line(&response);
    let _ = write_framed_response_with_timeout(&mut stream, &framed).await;
}

async fn cleanup_paths(server: &SandboxGatewayServer) {
    let _ = tokio::fs::remove_file(&server.config.pid_path).await;
}

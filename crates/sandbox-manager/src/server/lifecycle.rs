use std::sync::Arc;

use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::Semaphore;

use super::{SandboxManagerServer, ServerError};

impl SandboxManagerServer {
    pub async fn serve(self) -> Result<(), ServerError> {
        let server = Arc::new(self);
        prepare_paths(&server).await?;
        remove_file_if_exists(&server.config.socket_path).await?;
        let listener = UnixListener::bind(&server.config.socket_path)?;
        set_socket_permissions(&server).await?;
        tokio::fs::write(&server.config.pid_path, std::process::id().to_string()).await?;

        let permits = Arc::new(Semaphore::new(server.config.max_concurrent_connections));
        let result = accept_until_shutdown(Arc::clone(&server), listener, permits).await;
        cleanup_paths(&server).await;
        result
    }
}

async fn prepare_paths(server: &SandboxManagerServer) -> Result<(), ServerError> {
    if let Some(parent) = server.config.socket_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(parent) = server.config.pid_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    Ok(())
}

async fn accept_until_shutdown(
    server: Arc<SandboxManagerServer>,
    listener: UnixListener,
    permits: Arc<Semaphore>,
) -> Result<(), ServerError> {
    loop {
        tokio::select! {
            () = server.shutdown.cancelled() => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    tokio::spawn(reject_overloaded_connection(stream, server.config.max_concurrent_connections));
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
    let response = super::error::error_response(
        sandbox_protocol::error_kind::INTERNAL_ERROR,
        "manager is at connection capacity",
        serde_json::json!({ "max_concurrent_connections": max_connections }),
    );
    let _ = stream
        .write_all(&sandbox_protocol::response_line(&response))
        .await;
    let _ = stream.shutdown().await;
}

async fn set_socket_permissions(server: &SandboxManagerServer) -> Result<(), ServerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(
            &server.config.socket_path,
            std::fs::Permissions::from_mode(0o600),
        )
        .await?;
    }
    Ok(())
}

async fn cleanup_paths(server: &SandboxManagerServer) {
    let _ = tokio::fs::remove_file(&server.config.pid_path).await;
    let _ = tokio::fs::remove_file(&server.config.socket_path).await;
}

async fn remove_file_if_exists(path: &std::path::Path) -> Result<(), ServerError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

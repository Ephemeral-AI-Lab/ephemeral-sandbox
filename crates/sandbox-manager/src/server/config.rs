use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
    pub max_concurrent_connections: usize,
}

impl ServerConfig {
    #[must_use]
    pub fn new(
        socket_path: impl Into<PathBuf>,
        pid_path: impl Into<PathBuf>,
        max_concurrent_connections: usize,
    ) -> Self {
        Self {
            socket_path: socket_path.into(),
            pid_path: pid_path.into(),
            max_concurrent_connections,
        }
    }
}

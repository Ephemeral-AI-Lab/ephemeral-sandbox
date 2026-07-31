use std::io;

use sandbox_config::configs::gateway::GatewayEndpoint;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

pub(crate) trait GatewayIo: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> GatewayIo for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

pub(crate) type GatewayStream = Box<dyn GatewayIo>;

pub(crate) enum GatewayListener {
    Tcp(TcpListener),
    #[cfg(windows)]
    WindowsNamedPipe(windows::WindowsNamedPipeListener),
    #[cfg(unix)]
    UnixSocket(unix::UnixSocketListener),
}

impl GatewayListener {
    pub(crate) async fn bind(
        endpoint: GatewayEndpoint,
        max_connections: usize,
    ) -> io::Result<Self> {
        match endpoint {
            GatewayEndpoint::Tcp(address) => Ok(Self::Tcp(TcpListener::bind(address).await?)),
            GatewayEndpoint::WindowsNamedPipe(path) => {
                #[cfg(windows)]
                {
                    Ok(Self::WindowsNamedPipe(
                        windows::WindowsNamedPipeListener::bind(path, max_connections)?,
                    ))
                }
                #[cfg(not(windows))]
                {
                    let _ = (path, max_connections);
                    Err(unsupported_transport("npipe"))
                }
            }
            GatewayEndpoint::UnixSocket(path) => {
                #[cfg(unix)]
                {
                    Ok(Self::UnixSocket(unix::UnixSocketListener::bind(path)?))
                }
                #[cfg(not(unix))]
                {
                    let _ = (path, max_connections);
                    Err(unsupported_transport("unix"))
                }
            }
        }
    }

    pub(crate) async fn accept(&mut self) -> io::Result<GatewayStream> {
        match self {
            Self::Tcp(listener) => {
                let (stream, _) = listener.accept().await?;
                Ok(Box::new(stream))
            }
            #[cfg(windows)]
            Self::WindowsNamedPipe(listener) => listener.accept().await,
            #[cfg(unix)]
            Self::UnixSocket(listener) => listener.accept().await,
        }
    }
}

fn unsupported_transport(scheme: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{scheme} gateway endpoints are unsupported on this platform"),
    )
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use tokio::task::JoinSet;

    use super::GatewayStream;

    const MIN_PENDING_INSTANCES: usize = 8;
    const MAX_PENDING_INSTANCES: usize = 32;
    const ERROR_PIPE_BUSY: i32 = 231;

    pub(crate) struct WindowsNamedPipeListener {
        path: String,
        pending_target: usize,
        pending: JoinSet<io::Result<NamedPipeServer>>,
    }

    impl WindowsNamedPipeListener {
        pub(super) fn bind(path: String, max_connections: usize) -> io::Result<Self> {
            let pending_target = max_connections
                .saturating_add(1)
                .clamp(MIN_PENDING_INSTANCES, MAX_PENDING_INSTANCES);
            let mut listener = Self {
                path,
                pending_target,
                pending: JoinSet::new(),
            };
            let first = listener.create_instance(true)?;
            listener.spawn_pending(first);
            listener.replenish()?;
            Ok(listener)
        }

        pub(super) async fn accept(&mut self) -> io::Result<GatewayStream> {
            loop {
                match self.replenish() {
                    Ok(()) => {}
                    Err(error)
                        if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                            && !self.pending.is_empty() => {}
                    Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
                let joined = self.pending.join_next().await.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "named-pipe listener stopped")
                })?;
                let stream = joined.map_err(|error| {
                    io::Error::other(format!("named-pipe accept task failed: {error}"))
                })??;
                return Ok(Box::new(stream));
            }
        }

        fn replenish(&mut self) -> io::Result<()> {
            while self.pending.len() < self.pending_target {
                let instance = self.create_instance(false)?;
                self.spawn_pending(instance);
            }
            Ok(())
        }

        fn create_instance(&mut self, first: bool) -> io::Result<NamedPipeServer> {
            let mut options = ServerOptions::new();
            options
                .first_pipe_instance(first)
                .reject_remote_clients(true);
            options.create(&self.path)
        }

        fn spawn_pending(&mut self, instance: NamedPipeServer) {
            self.pending.spawn(async move {
                instance.connect().await?;
                Ok(instance)
            });
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::path::PathBuf;

    use tokio::net::UnixListener;

    use super::GatewayStream;

    pub(crate) struct UnixSocketListener {
        listener: UnixListener,
        path: PathBuf,
        identity: SocketIdentity,
    }

    impl UnixSocketListener {
        pub(super) fn bind(path: PathBuf) -> io::Result<Self> {
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("gateway Unix socket already exists: {}", path.display()),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let listener = UnixListener::bind(&path)?;
            if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
                let _ = fs::remove_file(&path);
                return Err(error);
            }
            let metadata = fs::symlink_metadata(&path)?;
            let identity = SocketIdentity::from_metadata(&metadata)?;
            Ok(Self {
                listener,
                path,
                identity,
            })
        }

        pub(super) async fn accept(&self) -> io::Result<GatewayStream> {
            let (stream, _) = self.listener.accept().await?;
            Ok(Box::new(stream))
        }
    }

    impl Drop for UnixSocketListener {
        fn drop(&mut self) {
            let Ok(metadata) = fs::symlink_metadata(&self.path) else {
                return;
            };
            let Ok(identity) = SocketIdentity::from_metadata(&metadata) else {
                return;
            };
            if identity == self.identity {
                let _ = fs::remove_file(&self.path);
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct SocketIdentity {
        device: u64,
        inode: u64,
    }

    impl SocketIdentity {
        fn from_metadata(metadata: &fs::Metadata) -> io::Result<Self> {
            if !metadata.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "gateway endpoint is not a Unix socket",
                ));
            }
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
    }
}

/// Enforcement thresholds for one framed request exchange. The protocol crate
/// owns the vocabulary and the shipped defaults; the daemon constructs this
/// from `daemon.server` at startup and threads it down its read path, while
/// pure clients rely on the associated defaults. This crate never reads
/// configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProtocolLimits {
    /// Byte cap for one request envelope (and, for clients, one response).
    pub max_request_bytes: usize,
    /// Deadline for reading one request line off an accepted connection.
    pub request_read_timeout_s: f64,
    /// Deadline for writing and closing one response line to a connected peer.
    pub response_write_timeout_s: f64,
}

impl ProtocolLimits {
    pub const DEFAULT_MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
    pub const DEFAULT_REQUEST_READ_TIMEOUT_S: f64 = 30.0;
    pub const DEFAULT_RESPONSE_WRITE_TIMEOUT_S: f64 = 30.0;
}

impl Default for ProtocolLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: Self::DEFAULT_MAX_REQUEST_BYTES,
            request_read_timeout_s: Self::DEFAULT_REQUEST_READ_TIMEOUT_S,
            response_write_timeout_s: Self::DEFAULT_RESPONSE_WRITE_TIMEOUT_S,
        }
    }
}

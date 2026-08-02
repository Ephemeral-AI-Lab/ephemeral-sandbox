use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use sandbox_operation_client::{ConfigError, GatewayConfig, GatewayConfigOverrides};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OperationSet {
    Management,
    Runtime,
    Observability,
}

#[derive(Debug, Parser)]
#[command(name = "sandbox-mcp")]
pub struct Cli {
    #[arg(long, value_enum)]
    pub set: OperationSet,

    #[arg(
        long = "gateway-endpoint",
        visible_alias = "gateway-socket",
        value_name = "URI"
    )]
    gateway_socket_path: Option<PathBuf>,

    #[arg(long = "gateway-auth-token", value_name = "TOKEN")]
    gateway_auth_token: Option<String>,
}

impl Cli {
    /// Resolve explicit gateway overrides through the shared client config path.
    ///
    /// # Errors
    /// Returns an error for an empty gateway address or authentication token.
    pub fn discover_gateway(&self) -> Result<GatewayConfig, ConfigError> {
        GatewayConfig::discover(GatewayConfigOverrides {
            gateway_socket_path: self.gateway_socket_path.clone(),
            gateway_auth_token: self.gateway_auth_token.clone(),
        })
    }
}

//! TLS transport (milestone 8).

use tokio::net::TcpStream;

use crate::error::{ClientError, Result};
use crate::session::SessionConfig;
use crate::transport::Transport;

pub async fn connect(_cfg: &SessionConfig, _host: &str, _tcp: TcpStream) -> Result<Transport> {
    Err(ClientError::Unsupported(
        "TLS is not implemented yet".into(),
    ))
}

use spice_proto::ProtoError;
use spice_proto::enums::link_err;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Proto(#[from] ProtoError),
    #[error("invalid URI: {0}")]
    Uri(#[from] crate::uri::UriError),
    #[error("{0}")]
    Config(String),
    #[error("could not connect to {addr}")]
    Connect {
        addr: String,
        #[source]
        source: std::io::Error,
    },
    #[error("connection to {0} timed out")]
    ConnectTimeout(String),
    #[error("server link error: {}", link_err::describe(*.0))]
    Link(u32),
    #[error("authentication failed: wrong or missing password")]
    AuthFailed,
    #[error(
        "password is longer than {} characters",
        spice_proto::enums::SPICE_MAX_PASSWORD_LENGTH
    )]
    PasswordTooLong,
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("{0}")]
    Unsupported(String),
}

impl ClientError {
    /// True when the server asked for (another) password.
    pub fn is_auth_failure(&self) -> bool {
        matches!(
            self,
            ClientError::AuthFailed | ClientError::Link(link_err::PERMISSION_DENIED)
        )
    }
}

pub type Result<T> = std::result::Result<T, ClientError>;

//! Byte transports for a channel: TCP, TLS over TCP, or a Unix socket.
//! Replaces `channel_connect` / `bio-gio.c` in spice-gtk.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use log::debug;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use crate::error::{ClientError, Result};

/// glib's `SOCKET_TIMEOUT` in spice-session.c is 10 seconds.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub enum Transport {
    Tcp(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

impl Transport {
    pub async fn connect_tcp(host: &str, port: u16) -> Result<TcpStream> {
        let addr = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        debug!("connecting to {addr}");
        let s = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| ClientError::ConnectTimeout(addr.clone()))?
            .map_err(|source| ClientError::Connect {
                addr: addr.clone(),
                source,
            })?;
        // spice-gtk sets TCP_NODELAY: input latency matters more than packet count.
        let _ = s.set_nodelay(true);
        Ok(s)
    }

    #[cfg(unix)]
    pub async fn connect_unix(path: &str) -> Result<Self> {
        let s = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::UnixStream::connect(path))
            .await
            .map_err(|_| ClientError::ConnectTimeout(path.to_string()))?
            .map_err(|source| ClientError::Connect {
                addr: path.to_string(),
                source,
            })?;
        Ok(Transport::Unix(s))
    }

    pub fn is_tls(&self) -> bool {
        matches!(self, Transport::Tls(_))
    }
}

macro_rules! delegate {
    ($self:ident, $s:ident => $e:expr) => {
        match $self.get_mut() {
            Transport::Tcp($s) => {
                let $s = Pin::new($s);
                $e
            }
            Transport::Tls($s) => {
                let $s = Pin::new(&mut **$s);
                $e
            }
            #[cfg(unix)]
            Transport::Unix($s) => {
                let $s = Pin::new($s);
                $e
            }
        }
    };
}

impl AsyncRead for Transport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        delegate!(self, s => s.poll_read(cx, buf))
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        delegate!(self, s => s.poll_write(cx, buf))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, s => s.poll_flush(cx))
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, s => s.poll_shutdown(cx))
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        delegate!(self, s => s.poll_write_vectored(cx, bufs))
    }
    fn is_write_vectored(&self) -> bool {
        match self {
            Transport::Tcp(s) => s.is_write_vectored(),
            Transport::Tls(s) => s.is_write_vectored(),
            #[cfg(unix)]
            Transport::Unix(s) => s.is_write_vectored(),
        }
    }
}

//! The generic part of every channel: connect, link, read messages, handle
//! base-channel messages (ping, set_ack, notify, wait_for_channels, ...) and
//! ACK flow control. Ports `spice-channel.c` and `channel-base.c`.
//!
//! Each channel runs as its own tokio task that reads and dispatches
//! messages. Writes go through an unbounded queue to a separate writer task,
//! so sending never blocks the reader.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use log::{Level, debug, info, log_enabled, trace, warn};
use spice_proto::caps::Caps;
use spice_proto::enums::{channel_type, msg, notify_severity};
use spice_proto::header::{DataHeader, sub_messages};
use spice_proto::msg::ClientMsg;
use spice_proto::msg::base::{
    Ack, AckSync, Disconnecting, Migrate, Notify, Ping, Pong, SetAck, WaitForChannels,
};
use spice_proto::{chan_log, warn_once_per};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, watch};

use crate::error::{ClientError, Result};
use crate::link::{self, LinkOutcome, Linked};
use crate::session::Shared;
use crate::transport::Transport;

/// Largest message body we accept (the C client has no limit; a sane cap
/// keeps a corrupt header from allocating gigabytes).
const MAX_MESSAGE_SIZE: u32 = 256 * 1024 * 1024;

/// How long `WAIT_FOR_CHANNELS` waits before giving up rather than deadlocking.
const WAIT_FOR_CHANNEL_TIMEOUT: Duration = Duration::from_secs(10);

/// A server message for a channel-specific handler.
#[derive(Debug, Clone)]
pub struct Message {
    pub ty: u16,
    pub body: Bytes,
}

struct OutMsg {
    ty: u16,
    body: Bytes,
}

/// Cloneable handle that queues client messages on a channel.
#[derive(Clone)]
pub struct MsgSender {
    tx: mpsc::UnboundedSender<OutMsg>,
}

impl MsgSender {
    pub fn send<M: ClientMsg>(&self, m: &M) {
        let mut b = BytesMut::new();
        m.encode(&mut b);
        self.send_raw(M::TYPE, b.freeze());
    }

    pub fn send_raw(&self, ty: u16, body: Bytes) {
        // A closed queue means the channel is going away; nothing to do.
        let _ = self.tx.send(OutMsg { ty, body });
    }
}

pub struct Channel {
    /// `"{type}-{id}"`, e.g. `display-0`.
    pub name: Arc<str>,
    pub channel_type: u8,
    pub id: u8,
    pub linked: Linked,
    pub tx: MsgSender,
    pub session: Arc<Shared>,
    rd: BufReader<ReadHalf<Transport>>,
    ack_window: u32,
    ack_count: u32,
    in_serial: u64,
    serial_tx: watch::Sender<u64>,
    pending: VecDeque<Message>,
    writer: tokio::task::JoinHandle<()>,
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.writer.abort();
        self.session.unregister_serial(self.channel_type, self.id);
    }
}

impl Channel {
    pub fn remote_has_cap(&self, cap: u32) -> bool {
        self.linked.channel_caps.test(cap)
    }

    /// Connect, link and authenticate a channel, choosing the plain or TLS
    /// port like `spice_session_channel_open_host` and retrying on TLS when
    /// the server answers `NEED_SECURED`.
    pub async fn open(session: &Arc<Shared>, ty: u8, id: u8, caps: Caps) -> Result<Channel> {
        let name: Arc<str> = format!("{}-{id}", channel_type::name(ty)).into();
        let cfg = &session.config;
        let mut use_tls = cfg.channel_needs_tls(ty);
        let uri = &cfg.uri;

        let (mut stream, linked) = loop {
            let stream = if let Some(path) = &uri.unix_path {
                if use_tls {
                    return Err(ClientError::Config("no TLS for Unix sockets".into()));
                }
                connect_unix(path).await?
            } else {
                let host = uri.connect_host().unwrap_or("localhost");
                let port = if use_tls { &uri.tls_port } else { &uri.port };
                let Some(port) = port else {
                    if !use_tls {
                        debug!("{name}: no plain port, trying the TLS port");
                        use_tls = true;
                        continue;
                    }
                    return Err(ClientError::Config(format!(
                        "{name} needs TLS but no tls-port was given"
                    )));
                };
                let port = crate::uri::SpiceUri::port_number(port)?;
                debug!(
                    "{name}: using {} port {port}",
                    if use_tls { "TLS" } else { "plain text" }
                );
                let tcp = Transport::connect_tcp(host, port).await?;
                if use_tls {
                    crate::tls::connect(cfg, host, tcp).await?
                } else {
                    Transport::Tcp(tcp)
                }
            };
            let mut stream = stream;
            let password = cfg.password.as_deref().unwrap_or("");
            match link::link(
                &mut stream,
                &name,
                session.connection_id(),
                ty,
                id,
                &caps,
                password,
            )
            .await?
            {
                LinkOutcome::Ok(l) => break (stream, l),
                LinkOutcome::NeedSecured if !use_tls => {
                    debug!("{name}: switching to TLS");
                    use_tls = true;
                }
                LinkOutcome::NeedSecured => {
                    return Err(ClientError::Link(
                        spice_proto::enums::link_err::NEED_SECURED,
                    ));
                }
            }
        };
        let _ = stream.flush().await;
        info!(
            "{name}: channel up{}",
            if stream.is_tls() { " (TLS)" } else { "" }
        );

        let (rd, wr) = tokio::io::split(stream);
        let (tx, rx) = mpsc::unbounded_channel();
        let writer = tokio::spawn(writer_task(name.clone(), wr, rx, linked.use_mini_header));
        let (serial_tx, serial_rx) = watch::channel(0);
        session.register_serial(ty, id, serial_rx);
        Ok(Channel {
            name,
            channel_type: ty,
            id,
            linked,
            tx: MsgSender { tx },
            session: session.clone(),
            rd: BufReader::with_capacity(64 * 1024, rd),
            ack_window: 0,
            ack_count: 0,
            in_serial: 0,
            serial_tx,
            pending: VecDeque::new(),
            writer,
        })
    }

    /// The next channel-specific message. Base-channel messages are handled
    /// here and never returned. `Ok(None)` means the server closed the
    /// connection cleanly.
    pub async fn next(&mut self) -> Result<Option<Message>> {
        loop {
            if let Some(m) = self.pending.pop_front() {
                return Ok(Some(m));
            }
            let mini = self.linked.use_mini_header;
            let mut hdr = [0u8; DataHeader::FULL_SIZE];
            let hdr = &mut hdr[..DataHeader::size(mini)];
            match self.rd.read_exact(hdr).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(e.into()),
            }
            let h = DataHeader::decode(hdr, mini)?;
            if h.size > MAX_MESSAGE_SIZE {
                return Err(ClientError::Proto(spice_proto::ProtoError::Other(format!(
                    "message of {} bytes is too large",
                    h.size
                ))));
            }
            let mut body = vec![0u8; h.size as usize];
            self.rd.read_exact(&mut body).await?;
            let body = Bytes::from(body);
            self.in_serial += 1;
            if log_enabled!(Level::Trace) {
                trace!(
                    "{}: << type {} size {} sub_list {}",
                    self.name, h.msg_type, h.size, h.sub_list
                );
            }

            let mut msgs = Vec::new();
            if h.msg_type == msg::base::LIST || h.sub_list != 0 {
                for (ty, sub) in sub_messages(&body, h.sub_list)? {
                    msgs.push(Message {
                        ty,
                        body: body.slice_ref(sub),
                    });
                }
            }

            // ACK flow control, counted per wire message as spice_channel_recv_msg does.
            if self.ack_count > 0 {
                self.ack_count -= 1;
                if self.ack_count == 0 {
                    self.tx.send(&Ack);
                    self.ack_count = self.ack_window;
                }
            }

            if h.msg_type != msg::base::LIST {
                msgs.push(Message {
                    ty: h.msg_type,
                    body,
                });
            }
            for m in msgs {
                if m.ty <= msg::base::BASE_LAST {
                    self.handle_base(m).await?;
                } else {
                    self.pending.push_back(m);
                }
            }

            // With the full header the server's serial is authoritative (it may skip).
            let serial = if mini { self.in_serial } else { h.serial };
            self.serial_tx.send_replace(serial);
        }
    }

    async fn handle_base(&mut self, m: Message) -> Result<()> {
        match m.ty {
            msg::base::SET_ACK => {
                let a = SetAck::decode(&m.body)?;
                chan_log!(
                    debug,
                    self,
                    "set ack: generation {} window {}",
                    a.generation,
                    a.window
                );
                self.ack_window = a.window;
                self.ack_count = a.window;
                self.tx.send(&AckSync {
                    generation: a.generation,
                });
            }
            msg::base::PING => {
                let p = Ping::decode(&m.body)?;
                chan_log!(
                    trace,
                    self,
                    "ping id {} ({} bytes of data)",
                    p.id,
                    p.data_len
                );
                self.tx.send(&Pong {
                    id: p.id,
                    timestamp: p.timestamp,
                });
            }
            msg::base::NOTIFY => {
                let n = Notify::decode(&m.body)?;
                let sev = match n.severity {
                    notify_severity::INFO => "info",
                    notify_severity::WARN => "warn",
                    notify_severity::ERROR => "error",
                    _ => "?",
                };
                if n.severity >= notify_severity::WARN {
                    chan_log!(
                        warn,
                        self,
                        "server notify ({sev} #{}): {}",
                        n.what,
                        n.message
                    );
                } else {
                    chan_log!(
                        debug,
                        self,
                        "server notify ({sev} #{}): {}",
                        n.what,
                        n.message
                    );
                }
            }
            msg::base::DISCONNECTING => {
                let d = Disconnecting::decode(&m.body)?;
                chan_log!(
                    debug,
                    self,
                    "server disconnecting: ts {} reason {}",
                    d.time_stamp,
                    d.reason
                );
            }
            msg::base::WAIT_FOR_CHANNELS => {
                let w = WaitForChannels::decode(&m.body)?;
                self.wait_for_channels(&w).await;
            }
            msg::base::MIGRATE => {
                let mig = Migrate::decode(&m.body)?;
                warn_once_per!(
                    self.channel_type,
                    "{}: migration is not supported (flags {:#x})",
                    self.name,
                    mig.flags
                );
            }
            msg::base::MIGRATE_DATA => {}
            other => {
                warn_once_per!(other, "{}: unhandled base message {other}", self.name);
            }
        }
        Ok(())
    }

    /// `spice_channel_handle_wait_for_channels`: wait until the listed
    /// channels have processed the given message serials.
    pub async fn wait_for_channels(&self, w: &WaitForChannels) {
        for (i, wait) in w.wait_list.iter().enumerate() {
            chan_log!(
                debug,
                self,
                "waiting for {}-{} serial {} ({}/{})",
                channel_type::name(wait.channel_type),
                wait.channel_id,
                wait.message_serial,
                i + 1,
                w.wait_list.len()
            );
            let Some(mut rx) = self
                .session
                .serial_watch(wait.channel_type, wait.channel_id)
            else {
                continue;
            };
            let target = wait.message_serial;
            let r =
                tokio::time::timeout(WAIT_FOR_CHANNEL_TIMEOUT, rx.wait_for(|s| *s >= target)).await;
            if r.is_err() {
                warn!(
                    "{}: timed out waiting for {}-{} serial {}",
                    self.name,
                    channel_type::name(wait.channel_type),
                    wait.channel_id,
                    target
                );
            }
        }
    }
}

async fn writer_task(
    name: Arc<str>,
    mut wr: WriteHalf<Transport>,
    mut rx: mpsc::UnboundedReceiver<OutMsg>,
    mini: bool,
) {
    let mut serial: u64 = 1;
    let mut buf = BytesMut::with_capacity(4096);
    while let Some(m) = rx.recv().await {
        buf.clear();
        let mut next = Some(m);
        // Coalesce whatever is already queued into one write.
        while let Some(m) = next {
            DataHeader {
                serial,
                msg_type: m.ty,
                size: m.body.len() as u32,
                sub_list: 0,
            }
            .encode(mini, &mut buf);
            buf.extend_from_slice(&m.body);
            if log_enabled!(Level::Trace) {
                trace!("{name}: >> type {} size {}", m.ty, m.body.len());
            }
            serial += 1;
            next = if buf.len() < 64 * 1024 {
                rx.try_recv().ok()
            } else {
                None
            };
        }
        if let Err(e) = async {
            wr.write_all(&buf).await?;
            wr.flush().await
        }
        .await
        {
            debug!("{name}: write failed: {e}");
            break;
        }
    }
}

#[cfg(unix)]
async fn connect_unix(path: &str) -> Result<Transport> {
    Transport::connect_unix(path).await
}

#[cfg(not(unix))]
async fn connect_unix(_: &str) -> Result<Transport> {
    Err(ClientError::Config(
        "spice+unix:// URIs are not supported on this platform".into(),
    ))
}

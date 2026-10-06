//! A SPICE session: connection parameters, the main channel, and the other
//! channels it announces. Ports the parts of `spice-session.c` a client needs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use log::{debug, info, warn};
use spice_proto::enums::channel_type;
use spice_proto::msg::main::ChannelId;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use crate::audio::AudioBackend;
use crate::channel::Channel;
use crate::cursor_ch::CursorChannel;
use crate::display_ch::DisplayChannel;
use crate::error::{ClientError, Result};
use crate::events::{ClientCommand, ClientEvent, EventSink};
use crate::inputs_ch::{InputCmd, InputsChannel};
use crate::main_ch::{MainChannel, MainCmd};
use crate::playback_ch::PlaybackChannel;
use crate::record_ch::RecordChannel;
use crate::uri::SpiceUri;

#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    /// PEM CA bundle; system roots when `None`.
    pub ca_file: Option<PathBuf>,
    /// Expected certificate subject, e.g. `C=GB,O=Example,CN=host`.
    pub host_subject: Option<String>,
}

#[derive(Clone)]
pub struct SessionConfig {
    pub uri: SpiceUri,
    pub password: Option<String>,
    pub tls: TlsOptions,
    /// Channel names that must use TLS, or `"all"`.
    pub secure_channels: Vec<String>,
    /// `SpiceImageCompression` to request, if any.
    pub preferred_compression: Option<u8>,
    pub enable_audio: bool,
    pub enable_record: bool,
    /// Resize the guest to follow the window (needs the agent).
    pub agent_resize: bool,
    /// Audio devices; without one, audio channels are not opened.
    pub audio: Option<Arc<dyn AudioBackend>>,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConfig")
            .field("uri", &self.uri)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("tls", &self.tls)
            .field("secure_channels", &self.secure_channels)
            .field("preferred_compression", &self.preferred_compression)
            .finish_non_exhaustive()
    }
}

impl SessionConfig {
    pub fn new(uri: SpiceUri) -> Self {
        let password = uri.password.clone();
        Self {
            uri,
            password,
            tls: TlsOptions::default(),
            secure_channels: Vec::new(),
            preferred_compression: None,
            enable_audio: true,
            enable_record: true,
            agent_resize: true,
            audio: None,
        }
    }

    pub fn channel_needs_tls(&self, ty: u8) -> bool {
        let name = channel_type::name(ty);
        self.secure_channels.iter().any(|c| c == "all" || c == name)
    }
}

/// The multimedia clock (`spice_session_set_mm_time` / `get_mm_time`): the
/// server's millisecond time, advanced locally between updates.
#[derive(Debug)]
struct MmTime {
    base: u32,
    at: Instant,
}

/// State shared by all channels of a session.
pub struct Shared {
    pub config: SessionConfig,
    pub events: Arc<dyn EventSink>,
    connection_id: AtomicU32,
    mm_time: Mutex<Option<MmTime>>,
    serials: Mutex<HashMap<(u8, u8), watch::Receiver<u64>>>,
    /// The GLZ dictionary shared by all display channels.
    pub glz_window: Arc<spice_canvas::GlzWindow>,
    /// Audio playback latency in ms, `u32::MAX` while no playback is active.
    playback_delay: AtomicU32,
}

impl Shared {
    pub fn connection_id(&self) -> u32 {
        self.connection_id.load(Ordering::Relaxed)
    }

    pub fn set_connection_id(&self, id: u32) {
        self.connection_id.store(id, Ordering::Relaxed);
    }

    pub fn emit(&self, ev: ClientEvent) {
        self.events.emit(ev);
    }

    pub fn set_mm_time(&self, time: u32) {
        let mut mm = self.mm_time.lock().unwrap();
        if let Some(old) = mm.as_ref() {
            let local = old.base.wrapping_add(old.at.elapsed().as_millis() as u32);
            let drift = time.wrapping_sub(local) as i32;
            if drift.unsigned_abs() > 1000 {
                debug!("mm-time jumped by {drift} ms");
            }
        }
        *mm = Some(MmTime {
            base: time,
            at: Instant::now(),
        });
    }

    /// Current multimedia time in ms, or `None` before the server sent one.
    pub fn mm_time(&self) -> Option<u32> {
        self.mm_time
            .lock()
            .unwrap()
            .as_ref()
            .map(|m| m.base.wrapping_add(m.at.elapsed().as_millis() as u32))
    }

    /// `spice_session_get_playback_latency`, or `None` when no playback
    /// is active (stream reports then send `UINT32_MAX`).
    pub fn playback_delay(&self) -> Option<u32> {
        let d = self.playback_delay.load(Ordering::Relaxed);
        (d != u32::MAX).then_some(d)
    }

    pub(crate) fn set_playback_delay(&self, d: Option<u32>) {
        let v = match d {
            Some(d) => d.min(u32::MAX - 1),
            None => u32::MAX,
        };
        self.playback_delay.store(v, Ordering::Relaxed);
    }

    pub(crate) fn register_serial(&self, ty: u8, id: u8, rx: watch::Receiver<u64>) {
        self.serials.lock().unwrap().insert((ty, id), rx);
    }

    pub(crate) fn unregister_serial(&self, ty: u8, id: u8) {
        self.serials.lock().unwrap().remove(&(ty, id));
    }

    pub(crate) fn serial_watch(&self, ty: u8, id: u8) -> Option<watch::Receiver<u64>> {
        self.serials.lock().unwrap().get(&(ty, id)).cloned()
    }
}

/// Requests from channel tasks back to the session.
pub(crate) enum SessionMsg {
    /// `SPICE_MSG_MAIN_CHANNELS_LIST`: open these channels.
    Channels(Vec<ChannelId>),
}

/// Run a session until the main channel closes or `Quit` is received.
///
/// Errors from connecting the main channel (bad address, wrong password, ...)
/// are returned before any other channel is opened, so the caller can retry,
/// for example after prompting for a password.
pub async fn run(
    config: SessionConfig,
    events: Arc<dyn EventSink>,
    commands: &mut mpsc::UnboundedReceiver<ClientCommand>,
) -> Result<()> {
    if config.uri.unix_path.is_some() && cfg!(not(unix)) {
        return Err(ClientError::Config(
            "spice+unix:// URIs are not supported on this platform".into(),
        ));
    }
    info!("connecting to {}", config.uri);
    let shared = Arc::new(Shared {
        config,
        events,
        connection_id: AtomicU32::new(0),
        mm_time: Mutex::new(None),
        serials: Mutex::new(HashMap::new()),
        glz_window: spice_canvas::GlzWindow::new(),
        playback_delay: AtomicU32::new(u32::MAX),
    });

    let main = Channel::open(&shared, channel_type::MAIN, 0, MainChannel::caps()).await?;
    shared.emit(ClientEvent::ChannelUp {
        channel_type: channel_type::MAIN,
        id: 0,
    });
    let (sess_tx, mut sess_rx) = mpsc::unbounded_channel();
    let (main_tx, main_rx) = mpsc::unbounded_channel::<MainCmd>();
    let mut main_task = tokio::spawn(MainChannel::new(main, sess_tx, main_rx).run());
    let mut channels: JoinSet<(u8, u8, Result<()>)> = JoinSet::new();
    let mut inputs_tx: Option<mpsc::UnboundedSender<InputCmd>> = None;

    let result = loop {
        tokio::select! {
            r = &mut main_task => {
                break match r {
                    Ok(r) => r,
                    Err(e) => Err(ClientError::Config(format!("main channel task failed: {e}"))),
                };
            }
            cmd = commands.recv() => match cmd {
                None | Some(ClientCommand::Quit) => {
                    info!("disconnecting");
                    break Ok(());
                }
                Some(ClientCommand::Input(i)) => {
                    if let Some(tx) = &inputs_tx {
                        let _ = tx.send(i);
                    }
                }
                Some(ClientCommand::DisplaySize { display, width, height }) => {
                    let _ = main_tx.send(MainCmd::DisplaySize { display, width, height });
                }
                Some(ClientCommand::ClipboardText(t)) => {
                    let _ = main_tx.send(MainCmd::ClipboardText(t));
                }
            },
            Some(m) = sess_rx.recv() => match m {
                SessionMsg::Channels(list) => {
                    for c in list {
                        if let Some(tx) = spawn_channel(&shared, &mut channels, c) {
                            inputs_tx = Some(tx);
                        }
                    }
                }
            },
            Some(done) = channels.join_next(), if !channels.is_empty() => {
                if let Ok((ty, id, r)) = done {
                    let error = r.err().map(|e| e.to_string());
                    match &error {
                        Some(e) => warn!("{}-{id}: channel closed: {e}", channel_type::name(ty)),
                        None => info!("{}-{id}: channel closed", channel_type::name(ty)),
                    }
                    shared.emit(ClientEvent::ChannelDown { channel_type: ty, id, error });
                }
            }
        }
    };

    main_task.abort();
    channels.abort_all();
    shared.emit(ClientEvent::ChannelDown {
        channel_type: channel_type::MAIN,
        id: 0,
        error: result.as_ref().err().map(|e| e.to_string()),
    });
    result
}

/// Start a channel task. Returns the command sender for an inputs channel.
fn spawn_channel(
    shared: &Arc<Shared>,
    set: &mut JoinSet<(u8, u8, Result<()>)>,
    c: ChannelId,
) -> Option<mpsc::UnboundedSender<InputCmd>> {
    let (ty, id) = (c.channel_type, c.id);
    let name = channel_type::name(ty);
    let shared = shared.clone();
    macro_rules! spawn {
        ($caps:expr, |$ch:ident| $run:expr) => {
            set.spawn(async move {
                let r = async {
                    let $ch = Channel::open(&shared, ty, id, $caps).await?;
                    shared.emit(ClientEvent::ChannelUp {
                        channel_type: ty,
                        id,
                    });
                    $run.await
                }
                .await;
                (ty, id, r)
            })
        };
    }
    match ty {
        channel_type::DISPLAY => {
            spawn!(DisplayChannel::caps(), |ch| DisplayChannel::new(ch).run());
        }
        channel_type::CURSOR => {
            spawn!(CursorChannel::caps(), |ch| CursorChannel::new(ch).run());
        }
        channel_type::PLAYBACK if shared.config.enable_audio && shared.config.audio.is_some() => {
            let be = shared.config.audio.clone();
            spawn!(PlaybackChannel::caps(), |ch| PlaybackChannel::new(ch, be)
                .run());
        }
        channel_type::RECORD if shared.config.enable_record && shared.config.audio.is_some() => {
            let be = shared.config.audio.clone();
            spawn!(RecordChannel::caps(), |ch| RecordChannel::new(ch, be).run());
        }
        channel_type::PLAYBACK | channel_type::RECORD => {
            info!("{name}-{id}: audio disabled; not opening");
        }
        channel_type::INPUTS => {
            let (tx, rx) = mpsc::unbounded_channel();
            spawn!(InputsChannel::caps(), |ch| InputsChannel::new(ch, rx).run());
            return Some(tx);
        }
        _ => info!("{name}-{id}: channel type not supported yet; ignoring"),
    }
    None
}

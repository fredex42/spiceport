//! The main channel (`channel-main.c`): session init, the channel list, mouse
//! mode, the multimedia clock and the VD agent (capabilities, monitors
//! config, text clipboard).

use std::collections::VecDeque;
use std::time::Duration;

use log::{info, warn};
use spice_proto::agent::{self, ClipboardMsg, MonConfig, cap, clipboard_type, msg_type, selection};
use spice_proto::caps::{self, Caps};
use spice_proto::enums::{channel_type, mouse_mode, msg};
use spice_proto::msg::main::{
    self as m, AgentData, AgentStart, AttachChannels, ChannelsList, Init, MouseMode,
    MouseModeRequest,
};
use spice_proto::{chan_log, warn_once_per};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::channel::{Channel, Message};
use crate::error::Result;
use crate::events::{ClientEvent, MouseMode as UiMouseMode};
use crate::session::SessionMsg;
use crate::text;

/// `MAX_DISPLAY`
const MAX_DISPLAY: usize = 16;
/// spice-gtk's default `max-clipboard` (100 MiB).
const MAX_CLIPBOARD: usize = 100 * 1024 * 1024;
/// `update_display_timer(channel, 1)`: wait for resizes to settle.
const MONITORS_CONFIG_DELAY: Duration = Duration::from_secs(1);

/// Commands routed to the main channel.
#[derive(Debug)]
pub enum MainCmd {
    /// The window showing `display` is now `width`x`height` pixels.
    DisplaySize {
        display: u8,
        width: u32,
        height: u32,
    },
    /// The client clipboard now holds this text.
    ClipboardText(String),
}

/// `SpiceDisplayConfig`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct DisplayConfig {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    enabled: bool,
}

pub struct MainChannel {
    ch: Channel,
    session_tx: mpsc::UnboundedSender<SessionMsg>,
    cmds: mpsc::UnboundedReceiver<MainCmd>,
    name: std::sync::Arc<str>,
    mouse_mode: u32,
    /// spice-gtk always asks for client mode when the server offers it.
    requested_mouse_mode: u32,

    agent_connected: bool,
    agent_caps_received: bool,
    agent_caps: Caps,
    agent_tokens: u32,
    /// `AGENT_DATA` payloads waiting for tokens.
    agent_queue: VecDeque<Vec<u8>>,
    agent_in: agent::Reassembler,
    display_config_sent: bool,

    displays: [DisplayConfig; MAX_DISPLAY],
    /// When to send the monitors config (debounced resizes).
    monitors_timer: Option<Instant>,

    clipboard_serial: [u32; 3],
    /// What the client clipboard holds, offered to the guest on request.
    client_text: Option<String>,
}

impl MainChannel {
    /// Channel caps, as `spice_main_channel_reset_capabilties` sets them
    /// (minus migration, which we don't support).
    pub fn caps() -> Caps {
        Caps::new()
            .with(caps::main::NAME_AND_UUID)
            .with(caps::main::AGENT_CONNECTED_TOKENS)
    }

    pub(crate) fn new(
        ch: Channel,
        session_tx: mpsc::UnboundedSender<SessionMsg>,
        cmds: mpsc::UnboundedReceiver<MainCmd>,
    ) -> Self {
        let name = ch.name.clone();
        Self {
            ch,
            session_tx,
            cmds,
            name,
            mouse_mode: 0,
            requested_mouse_mode: mouse_mode::CLIENT,
            agent_connected: false,
            agent_caps_received: false,
            agent_caps: Caps::new(),
            agent_tokens: 0,
            agent_queue: VecDeque::new(),
            agent_in: agent::Reassembler::default(),
            display_config_sent: false,
            displays: [DisplayConfig::default(); MAX_DISPLAY],
            monitors_timer: None,
            clipboard_serial: [0; 3],
            client_text: None,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        loop {
            let timer = self.monitors_timer;
            tokio::select! {
                m = self.ch.next() => match m? {
                    Some(m) => self.handle(m)?,
                    None => {
                        info!("{}: server closed the connection", self.name);
                        return Ok(());
                    }
                },
                Some(cmd) = self.cmds.recv() => self.command(cmd),
                _ = tokio::time::sleep_until(timer.unwrap_or_else(Instant::now)), if timer.is_some() => {
                    self.monitors_timer = None;
                    self.timer_set_display();
                }
            }
        }
    }

    fn handle(&mut self, msg: Message) -> Result<()> {
        let b = &msg.body;
        match msg.ty {
            msg::main::INIT => {
                let init = Init::decode(b)?;
                chan_log!(
                    debug,
                    self,
                    "init: session id {:#x}, {} display hint, mouse modes {}/{}, agent {}, {} tokens",
                    init.session_id,
                    init.display_channels_hint,
                    init.supported_mouse_modes,
                    init.current_mouse_mode,
                    init.agent_connected,
                    init.agent_tokens
                );
                self.ch.session.set_connection_id(init.session_id);
                self.set_mouse_mode(init.supported_mouse_modes, init.current_mouse_mode);
                self.ch.session.set_mm_time(init.multi_media_time);
                self.agent_tokens = init.agent_tokens;
                if init.agent_connected != 0 {
                    self.agent_start();
                }
                self.ch.tx.send(&AttachChannels);
            }
            msg::main::CHANNELS_LIST => {
                let list = ChannelsList::decode(b)?;
                let names: Vec<String> = list
                    .channels
                    .iter()
                    .map(|c| format!("{}-{}", channel_type::name(c.channel_type), c.id))
                    .collect();
                info!("{}: server channels: {}", self.name, names.join(", "));
                let _ = self.session_tx.send(SessionMsg::Channels(list.channels));
            }
            msg::main::MOUSE_MODE => {
                let mm = MouseMode::decode(b)?;
                self.set_mouse_mode(mm.supported_modes as u32, mm.current_mode as u32);
            }
            msg::main::MULTI_MEDIA_TIME => {
                self.ch.session.set_mm_time(m::decode_u32(b)?);
            }
            msg::main::NAME => {
                let name = m::decode_name(b)?;
                info!("{}: server name: {name}", self.name);
                self.ch.session.emit(ClientEvent::Name(name));
            }
            msg::main::UUID => {
                let uuid = m::decode_uuid(b)?;
                info!("{}: server uuid: {}", self.name, m::uuid_to_string(&uuid));
            }
            msg::main::AGENT_CONNECTED => self.agent_start(),
            msg::main::AGENT_CONNECTED_TOKENS => {
                self.agent_tokens = m::decode_u32(b)?;
                self.agent_start();
            }
            msg::main::AGENT_DISCONNECTED => {
                let err = m::decode_u32(b)?;
                chan_log!(debug, self, "agent disconnected (error {err})");
                self.set_agent_connected(false);
            }
            msg::main::AGENT_TOKEN => {
                self.agent_tokens = self.agent_tokens.saturating_add(m::decode_u32(b)?);
                self.agent_send_queue();
            }
            msg::main::AGENT_DATA => {
                if !self.agent_connected {
                    chan_log!(debug, self, "agent data while the agent is disconnected");
                }
                match self.agent_in.push(b) {
                    Ok(msgs) => {
                        for (h, body) in msgs {
                            self.agent_message(h, &body);
                        }
                    }
                    Err(e) => {
                        warn!("{}: bad agent data: {e}", self.name);
                        self.agent_in.reset();
                    }
                }
            }
            msg::main::MIGRATE_BEGIN
            | msg::main::MIGRATE_BEGIN_SEAMLESS
            | msg::main::MIGRATE_CANCEL
            | msg::main::MIGRATE_END
            | msg::main::MIGRATE_SWITCH_HOST
            | msg::main::MIGRATE_DST_SEAMLESS_ACK
            | msg::main::MIGRATE_DST_SEAMLESS_NACK => {
                warn_once_per!(
                    msg.ty,
                    "{}: migration message {} not supported",
                    self.name,
                    msg.ty
                );
            }
            other => {
                warn_once_per!(other, "{}: unhandled message {other}", self.name);
            }
        }
        Ok(())
    }

    fn command(&mut self, cmd: MainCmd) {
        match cmd {
            MainCmd::DisplaySize {
                display,
                width,
                height,
            } => {
                let id = display as usize;
                if id >= MAX_DISPLAY || width == 0 || height == 0 {
                    return;
                }
                let cfg = DisplayConfig {
                    x: self.displays[id].x,
                    y: self.displays[id].y,
                    width,
                    height,
                    enabled: true,
                };
                // update_display_config: only changes restart the timer.
                if self.displays[id] != cfg {
                    self.displays[id] = cfg;
                    if self.ch.session.config.agent_resize {
                        self.monitors_timer = Some(Instant::now() + MONITORS_CONFIG_DELAY);
                    }
                }
            }
            MainCmd::ClipboardText(t) => {
                self.client_text = Some(t);
                self.clipboard_grab();
            }
        }
    }

    /// `set_mouse_mode` in channel-main.c.
    fn set_mouse_mode(&mut self, supported: u32, current: u32) {
        if self.mouse_mode != current {
            self.mouse_mode = current;
            let mode = UiMouseMode::from_wire(current);
            info!("{}: mouse mode is now {mode:?}", self.name);
            self.ch.session.emit(ClientEvent::MouseMode(mode));
        }
        if self.requested_mouse_mode != self.mouse_mode
            && self.requested_mouse_mode & supported != 0
        {
            chan_log!(
                debug,
                self,
                "requesting mouse mode {}",
                self.requested_mouse_mode
            );
            self.ch.tx.send(&MouseModeRequest {
                mode: self.requested_mouse_mode as u16,
            });
        }
    }

    // ---- agent -----------------------------------------------------------

    fn agent_has(&self, c: u32) -> bool {
        self.agent_caps.test(c)
    }

    /// `agent_start`
    fn agent_start(&mut self) {
        self.agent_caps_received = false;
        self.set_agent_connected(true);
        self.ch.tx.send(&AgentStart { num_tokens: !0 });
        self.agent_announce_caps();
        self.agent_send_queue();
    }

    fn set_agent_connected(&mut self, connected: bool) {
        if self.agent_connected != connected {
            self.agent_connected = connected;
            info!(
                "{}: agent {}",
                self.name,
                if connected {
                    "connected"
                } else {
                    "disconnected"
                }
            );
            self.ch.session.emit(ClientEvent::AgentConnected(connected));
        }
        if !connected {
            // spice_main_channel_reset_agent
            self.agent_caps_received = false;
            self.agent_caps = Caps::new();
            self.display_config_sent = false;
            self.agent_in.reset();
            self.clipboard_serial = [0; 3];
        }
    }

    /// Queue an agent message (`agent_msg_queue`); it goes out as tokens allow.
    fn agent_queue_msg(&mut self, ty: u32, body: &[u8]) {
        self.agent_queue.extend(agent::chunk_message(ty, body));
    }

    /// `agent_send_msg_queue`: one token per `AGENT_DATA` message.
    fn agent_send_queue(&mut self) {
        while self.agent_tokens > 0 {
            let Some(chunk) = self.agent_queue.pop_front() else {
                break;
            };
            self.agent_tokens -= 1;
            self.ch.tx.send(&AgentData(&chunk));
        }
    }

    /// `agent_announce_caps`
    fn agent_announce_caps(&mut self) {
        if !self.agent_connected {
            return;
        }
        let caps = Caps::new()
            .with(cap::MOUSE_STATE)
            .with(cap::MONITORS_CONFIG)
            .with(cap::REPLY)
            .with(cap::DISPLAY_CONFIG)
            .with(cap::CLIPBOARD_BY_DEMAND)
            .with(cap::CLIPBOARD_SELECTION)
            .with(cap::MONITORS_CONFIG_POSITION)
            .with(cap::CLIPBOARD_NO_RELEASE_ON_REGRAB)
            .with(cap::CLIPBOARD_GRAB_SERIAL);
        let body = agent::encode_announce_caps(!self.agent_caps_received, &caps);
        self.agent_queue_msg(msg_type::ANNOUNCE_CAPABILITIES, &body);
    }

    /// `main_agent_handle_msg`
    fn agent_message(&mut self, h: agent::MessageHeader, body: &[u8]) {
        if h.protocol != agent::VD_AGENT_PROTOCOL {
            warn!("{}: agent protocol {} not supported", self.name, h.protocol);
            return;
        }
        match h.ty {
            msg_type::ANNOUNCE_CAPABILITIES => {
                let Ok((request, caps)) = agent::decode_announce_caps(body) else {
                    warn!("{}: bad agent capabilities", self.name);
                    return;
                };
                chan_log!(
                    debug,
                    self,
                    "agent caps: {:?}",
                    caps.iter().collect::<Vec<_>>()
                );
                self.agent_caps = caps;
                self.agent_caps_received = true;
                // update_display_timer(channel, 0)
                self.monitors_timer = Some(Instant::now());
                if request {
                    self.agent_announce_caps();
                }
                if self.agent_has(cap::DISPLAY_CONFIG) && !self.display_config_sent {
                    // VDAgentDisplayConfig with no flags set.
                    self.agent_queue_msg(msg_type::DISPLAY_CONFIG, &[0; 8]);
                    self.display_config_sent = true;
                }
                if self.agent_has(cap::MAX_CLIPBOARD) {
                    self.agent_queue_msg(
                        msg_type::MAX_CLIPBOARD,
                        &agent::encode_max_clipboard(MAX_CLIPBOARD as i32),
                    );
                }
                // Offer what the client clipboard already holds.
                if self.client_text.is_some() {
                    self.clipboard_grab();
                }
                self.agent_send_queue();
            }
            msg_type::CLIPBOARD
            | msg_type::CLIPBOARD_GRAB
            | msg_type::CLIPBOARD_REQUEST
            | msg_type::CLIPBOARD_RELEASE => self.agent_clipboard(h.ty, body),
            msg_type::REPLY => {
                if let Ok((ty, err)) = agent::decode_reply(body) {
                    chan_log!(
                        debug,
                        self,
                        "agent reply: type {ty}, {}",
                        if err == 1 { "success" } else { "error" }
                    );
                }
            }
            other => {
                warn_once_per!(other, "{}: unhandled agent message type {other}", self.name);
            }
        }
    }

    fn agent_clipboard(&mut self, ty: u32, body: &[u8]) {
        let with_sel = self.agent_has(cap::CLIPBOARD_SELECTION);
        let parsed = agent::clipboard_selection(body, with_sel).and_then(|(sel, rest)| {
            Ok((
                sel,
                agent::decode_clipboard(ty, rest, self.agent_has(cap::CLIPBOARD_GRAB_SERIAL))?,
            ))
        });
        let (sel, cb) = match parsed {
            Ok(v) => v,
            Err(e) => {
                warn!("{}: bad clipboard message: {e}", self.name);
                return;
            }
        };
        if sel != selection::CLIPBOARD {
            // Only the CLIPBOARD selection is synced for now.
            return;
        }
        let crlf = self.agent_has(cap::GUEST_LINEEND_CRLF);
        match cb {
            ClipboardMsg::Grab { serial, types } => {
                if let Some(s) = serial {
                    let ours = &mut self.clipboard_serial[sel as usize];
                    if s == *ours {
                        *ours = ours.wrapping_add(1);
                    } else {
                        chan_log!(debug, self, "grab discard, serial {s} != {ours}");
                        return;
                    }
                }
                chan_log!(debug, self, "guest clipboard grab: types {types:?}");
                if types.contains(&clipboard_type::UTF8_TEXT) {
                    // Fetch it now; the SDL clipboard isn't lazy.
                    let req = agent::encode_clipboard_request(
                        with_sel.then_some(sel),
                        clipboard_type::UTF8_TEXT,
                    );
                    self.agent_queue_msg(msg_type::CLIPBOARD_REQUEST, &req);
                    self.agent_send_queue();
                }
            }
            ClipboardMsg::Data { ty, data } => {
                if ty == clipboard_type::UTF8_TEXT {
                    let t = String::from_utf8_lossy(data);
                    let t = if crlf {
                        text::dos2unix(&t)
                    } else {
                        t.strip_suffix('\0').unwrap_or(&t).to_string()
                    };
                    chan_log!(debug, self, "guest clipboard text: {} bytes", t.len());
                    self.client_text = Some(t.clone());
                    self.ch.session.emit(ClientEvent::ClipboardText(t));
                }
            }
            ClipboardMsg::Request { ty } => {
                // clipboard_received_text_cb: reply even if we have nothing.
                let data = match (&self.client_text, ty) {
                    (Some(t), clipboard_type::UTF8_TEXT) if t.len() < MAX_CLIPBOARD => {
                        if crlf {
                            text::unix2dos(t)
                        } else {
                            t.clone()
                        }
                    }
                    _ => String::new(),
                };
                let body = agent::encode_clipboard(
                    with_sel.then_some(sel),
                    clipboard_type::UTF8_TEXT,
                    data.as_bytes(),
                );
                self.agent_queue_msg(msg_type::CLIPBOARD, &body);
                self.agent_send_queue();
            }
            ClipboardMsg::Release => {
                chan_log!(debug, self, "guest released the clipboard");
            }
        }
    }

    /// `agent_clipboard_grab`: tell the guest the client has text.
    fn clipboard_grab(&mut self) {
        if !self.agent_connected
            || !self.agent_caps_received
            || !self.agent_has(cap::CLIPBOARD_BY_DEMAND)
        {
            return;
        }
        let sel = selection::CLIPBOARD;
        let with_sel = self.agent_has(cap::CLIPBOARD_SELECTION);
        let serial = if self.agent_has(cap::CLIPBOARD_GRAB_SERIAL) {
            let s = self.clipboard_serial[sel as usize];
            self.clipboard_serial[sel as usize] = s.wrapping_add(1);
            Some(s)
        } else {
            None
        };
        let body = agent::encode_clipboard_grab(
            with_sel.then_some(sel),
            serial,
            &[clipboard_type::UTF8_TEXT],
        );
        self.agent_queue_msg(msg_type::CLIPBOARD_GRAB, &body);
        self.agent_send_queue();
    }

    /// `timer_set_display`
    fn timer_set_display(&mut self) {
        if !self.agent_connected
            || !self.agent_caps_received
            || !self.ch.session.config.agent_resize
        {
            return;
        }
        if !self.displays.iter().any(|d| d.width > 0 && d.height > 0) {
            chan_log!(
                debug,
                self,
                "not sending monitors config: no display has dimensions"
            );
            return;
        }
        if !self.agent_has(cap::MONITORS_CONFIG) {
            return;
        }
        self.send_monitors_config();
    }

    /// `spice_main_channel_send_monitor_config`
    fn send_monitors_config(&mut self) {
        let sparse = self.agent_has(cap::SPARSE_MONITORS_CONFIG);
        let mut monitors: Vec<MonConfig> = Vec::new();
        for d in &self.displays {
            if !d.enabled {
                if sparse {
                    monitors.push(MonConfig::default());
                }
                continue;
            }
            monitors.push(MonConfig {
                width: d.width,
                height: d.height,
                depth: 32,
                x: d.x,
                y: d.y,
            });
        }
        // monitors_align: lay enabled monitors out left to right.
        let mut x = 0;
        for mc in monitors.iter_mut() {
            mc.x = x;
            mc.y = 0;
            x += mc.width as i32;
        }
        let flags = agent::monitors_flag::USE_POS | agent::monitors_flag::PHYSICAL_SIZE;
        for (i, mc) in monitors.iter().enumerate().filter(|(_, m)| m.width > 0) {
            chan_log!(
                info,
                self,
                "monitor #{i}: {}x{}+{}+{}, asking the guest to resize",
                mc.width,
                mc.height,
                mc.x,
                mc.y
            );
        }
        let body = agent::encode_monitors_config(flags, &monitors, &[]);
        self.agent_queue_msg(msg_type::MONITORS_CONFIG, &body);
        self.agent_send_queue();
    }
}

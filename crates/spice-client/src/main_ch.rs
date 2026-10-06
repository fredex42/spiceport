//! The main channel (`channel-main.c`): session init, the channel list, mouse
//! mode, the multimedia clock and the VD agent connection.

use log::info;
use spice_proto::caps::{self, Caps};
use spice_proto::enums::{channel_type, mouse_mode, msg};
use spice_proto::msg::main::{
    self as m, AgentStart, AttachChannels, ChannelsList, Init, MouseMode, MouseModeRequest,
};
use spice_proto::{chan_log, warn_once_per};
use tokio::sync::mpsc;

use crate::channel::{Channel, Message};
use crate::error::Result;
use crate::events::{ClientEvent, MouseMode as UiMouseMode};
use crate::session::SessionMsg;

/// Commands routed to the main channel.
#[derive(Debug)]
pub enum MainCmd {}

pub struct MainChannel {
    ch: Channel,
    session_tx: mpsc::UnboundedSender<SessionMsg>,
    cmds: mpsc::UnboundedReceiver<MainCmd>,
    mouse_mode: u32,
    /// spice-gtk always asks for client mode when the server offers it.
    requested_mouse_mode: u32,
    agent_connected: bool,
    agent_tokens: u32,
    name: std::sync::Arc<str>,
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
            mouse_mode: 0,
            requested_mouse_mode: mouse_mode::CLIENT,
            agent_connected: false,
            agent_tokens: 0,
            name,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                m = self.ch.next() => match m? {
                    Some(m) => self.handle(m)?,
                    None => {
                        info!("{}: server closed the connection", self.name);
                        return Ok(());
                    }
                },
                Some(cmd) = self.cmds.recv() => match cmd {},
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
                info!("{}: agent disconnected (error {err})", self.name);
                self.set_agent_connected(false);
            }
            msg::main::AGENT_TOKEN => {
                self.agent_tokens += m::decode_u32(b)?;
            }
            msg::main::AGENT_DATA => {
                chan_log!(trace, self, "agent data: {} bytes", b.len());
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

    /// `agent_start`
    fn agent_start(&mut self) {
        self.set_agent_connected(true);
        self.ch.tx.send(&AgentStart { num_tokens: !0 });
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
    }
}

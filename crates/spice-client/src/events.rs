//! What the client tells the frontend ([`ClientEvent`]) and what the frontend
//! asks of the client ([`ClientCommand`]). These replace spice-gtk's GObject
//! signals and properties.

/// `SpiceMouseMode`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    /// Relative motion; the server draws the cursor. Needs a pointer grab.
    Server,
    /// Absolute positions; usually means an agent or tablet is present.
    Client,
}

impl MouseMode {
    pub fn from_wire(v: u32) -> Self {
        if v & spice_proto::enums::mouse_mode::CLIENT != 0 {
            MouseMode::Client
        } else {
            MouseMode::Server
        }
    }
}

#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// `SPICE_MSG_MAIN_NAME`: the VM's name, for the window title.
    Name(String),
    /// A channel finished its handshake.
    ChannelUp {
        channel_type: u8,
        id: u8,
    },
    /// A channel closed. `error` is set when it closed abnormally.
    ChannelDown {
        channel_type: u8,
        id: u8,
        error: Option<String>,
    },
    MouseMode(MouseMode),
    AgentConnected(bool),
}

#[derive(Debug, Clone)]
pub enum ClientCommand {
    /// Close the session.
    Quit,
}

/// Receives client events. Called from tokio worker threads, so it must not
/// block for long.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, ev: ClientEvent);
}

impl<F: Fn(ClientEvent) + Send + Sync + 'static> EventSink for F {
    fn emit(&self, ev: ClientEvent) {
        self(ev)
    }
}

//! What the client tells the frontend ([`ClientEvent`]) and what the frontend
//! asks of the client ([`ClientCommand`]). These replace spice-gtk's GObject
//! signals and properties.

use std::sync::Arc;

pub use crate::cursor::CursorShape;
pub use crate::inputs_ch::InputCmd;
pub use spice_canvas::{SharedSurface, SurfaceData};

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
    /// A display's primary surface was created (or replaced). The frontend
    /// keeps the handle and reads pixels from it on `Invalidate`.
    DisplayPrimary {
        display: u8,
        surface: SharedSurface,
        width: u32,
        height: u32,
    },
    /// The display's primary surface went away.
    DisplayPrimaryDestroyed {
        display: u8,
    },
    /// Pixels of the primary surface changed; the dirty area is in the
    /// surface. Further draws send no new event until the frontend clears
    /// `notify_pending` while uploading.
    Invalidate {
        display: u8,
    },
    /// `SPICE_MSG_DISPLAY_MARK` / `RESET`: whether the display has content.
    DisplayMark {
        display: u8,
        mark: bool,
    },
    /// `SPICE_MSG_DISPLAY_MONITORS_CONFIG`
    MonitorsConfig {
        display: u8,
        max_allowed: u16,
        heads: Vec<spice_proto::msg::display::Head>,
    },
    /// New cursor shape; `None` means "use the default cursor".
    CursorSet(Option<Arc<CursorShape>>),
    CursorHide,
    /// Server mouse mode: where the guest's cursor is.
    CursorMove {
        x: i32,
        y: i32,
    },
    CursorReset,
    /// The guest's lock-key state (`SpiceKeyboardModifierFlags`).
    KeyModifiers(u16),
    /// The guest copied text; put it on the client clipboard.
    ClipboardText(String),
}

#[derive(Debug, Clone)]
pub enum ClientCommand {
    /// Close the session.
    Quit,
    /// Keyboard and mouse input for the inputs channel.
    Input(InputCmd),
    /// The window for `display` is now this many pixels; with the agent,
    /// the guest is asked to match (debounced by a second).
    DisplaySize {
        display: u8,
        width: u32,
        height: u32,
    },
    /// The client clipboard now holds this text.
    ClipboardText(String),
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

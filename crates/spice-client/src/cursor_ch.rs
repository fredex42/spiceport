//! The cursor channel (`channel-cursor.c`).

use std::collections::HashMap;
use std::sync::Arc;

use log::warn;
use spice_proto::caps::Caps;
use spice_proto::enums::{cursor_flags, msg};
use spice_proto::msg::cursor::{self as c, Cursor};
use spice_proto::{chan_log, warn_once_per};

use crate::channel::{Channel, Message};
use crate::cursor::{self, CursorError, CursorShape};
use crate::error::Result;
use crate::events::ClientEvent;

pub struct CursorChannel {
    ch: Channel,
    name: Arc<str>,
    cache: HashMap<u64, Option<Arc<CursorShape>>>,
    init_done: bool,
}

impl CursorChannel {
    pub fn caps() -> Caps {
        Caps::new()
    }

    pub fn new(ch: Channel) -> Self {
        let name = ch.name.clone();
        Self {
            ch,
            name,
            cache: HashMap::new(),
            init_done: false,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        while let Some(m) = self.ch.next().await? {
            self.handle(m)?;
        }
        Ok(())
    }

    fn emit(&self, ev: ClientEvent) {
        self.ch.session.emit(ev);
    }

    /// `set_cursor`: `Ok(None)` means "no cursor" (hide); `Ok(Some(None))`
    /// means a cursor we can't convert (use the default).
    fn set_cursor(&mut self, cur: &Cursor<'_>) -> Option<Option<Arc<CursorShape>>> {
        if cur.flags & cursor_flags::NONE != 0 {
            return None;
        }
        let hdr = cur.header?;
        chan_log!(
            debug,
            self,
            "cursor: flags {:#x}, type {}, {:#x}, {}x{}",
            cur.flags,
            hdr.ty,
            hdr.unique,
            hdr.width,
            hdr.height
        );
        if cur.flags & cursor_flags::FROM_CACHE != 0 {
            let c = self.cache.get(&hdr.unique).cloned();
            if c.is_none() {
                warn!("{}: cursor {:#x} not in cache", self.name, hdr.unique);
            }
            return c;
        }
        if cur.data.is_empty() {
            return None;
        }
        let shape = match cursor::convert(&hdr, cur.data) {
            Ok(s) => Some(Arc::new(s)),
            Err(CursorError::Unsupported(t)) => {
                warn_once_per!(t, "{}: unimplemented cursor type {t}", self.name);
                None
            }
            Err(CursorError::Truncated) => {
                warn!("{}: truncated cursor data", self.name);
                return None;
            }
        };
        if cur.flags & cursor_flags::CACHE_ME != 0 {
            self.cache.insert(hdr.unique, shape.clone());
        }
        Some(shape)
    }

    fn handle(&mut self, m: Message) -> Result<()> {
        let b = &m.body;
        match m.ty {
            msg::cursor::INIT => {
                let init = c::Init::decode(b)?;
                self.cache.clear();
                let cursor = self.set_cursor(&init.cursor);
                self.init_done = true;
                if let Some(shape) = &cursor {
                    self.emit(ClientEvent::CursorSet(shape.clone()));
                }
                if !init.visible || cursor.is_none() {
                    self.emit(ClientEvent::CursorHide);
                }
            }
            msg::cursor::RESET => {
                chan_log!(debug, self, "reset");
                self.cache.clear();
                self.init_done = false;
                self.emit(ClientEvent::CursorReset);
            }
            msg::cursor::SET => {
                let set = c::Set::decode(b)?;
                match self.set_cursor(&set.cursor) {
                    Some(shape) => self.emit(ClientEvent::CursorSet(shape)),
                    None => self.emit(ClientEvent::CursorHide),
                }
            }
            msg::cursor::MOVE => {
                let p = c::decode_move(b)?;
                self.emit(ClientEvent::CursorMove {
                    x: p.x as i32,
                    y: p.y as i32,
                });
            }
            msg::cursor::HIDE => self.emit(ClientEvent::CursorHide),
            msg::cursor::TRAIL => {
                warn_once_per!(m.ty, "{}: cursor trails are not supported", self.name)
            }
            msg::cursor::INVAL_ONE => {
                self.cache.remove(&c::decode_inval_one(b)?);
            }
            msg::cursor::INVAL_ALL => self.cache.clear(),
            other => warn_once_per!(other, "{}: unhandled message {other}", self.name),
        }
        Ok(())
    }
}

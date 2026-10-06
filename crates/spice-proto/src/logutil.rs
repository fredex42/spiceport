//! Logging helpers shared by the spiceport crates.

/// Log a warning only the first time this call site runs.
#[macro_export]
macro_rules! warn_once {
    ($($arg:tt)*) => {{
        static ONCE: ::std::sync::Once = ::std::sync::Once::new();
        ONCE.call_once(|| ::log::warn!($($arg)*));
    }};
}

/// Log a warning the first time this call site sees `key` (e.g. a message
/// type or draw op), so each unsupported thing is reported once.
#[macro_export]
macro_rules! warn_once_per {
    ($key:expr, $($arg:tt)*) => {{
        static SEEN: ::std::sync::Mutex<Vec<u64>> = ::std::sync::Mutex::new(Vec::new());
        let k = ($key) as u64;
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.contains(&k) {
            seen.push(k);
            drop(seen);
            ::log::warn!($($arg)*);
        }
    }};
}

/// `"{type}-{id}: ..."` prefixed logging, like spice-gtk's `CHANNEL_DEBUG`.
/// The first argument is anything with a `name` field that implements Display.
#[macro_export]
macro_rules! chan_log {
    ($lvl:ident, $c:expr, $($arg:tt)*) => {
        ::log::$lvl!("{}: {}", $c.name, format_args!($($arg)*))
    };
}

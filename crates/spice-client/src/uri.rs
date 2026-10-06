//! SPICE URI parsing, ported from `spice_parse_uri` / `spice_uri_create` in
//! spice-gtk `spice-session.c`, including its quirks:
//!
//! - `spice://[user@]host[:port][/][?|;]key=value[&|;]...`
//! - keys `port`, `tls-port`, `password`; a key with an empty value is unset;
//!   unknown or repeated keys are errors;
//! - `spice+tls://host:port` puts the authority port in `tls_port` and ignores
//!   (with a warning) `port`/`tls-port` query keys;
//! - `spice+unix://` takes everything after the scheme, verbatim, as the path.
//!
//! The password never appears in `Display` or `Debug` output.

use std::fmt;

use log::warn;
use thiserror::Error;

const SCHEME_SPICE: &str = "spice://";
const SCHEME_SPICE_UNIX: &str = "spice+unix://";
const SCHEME_SPICE_TLS: &str = "spice+tls://";
const QUERY_START: &[char] = &[';', '?'];
const QUERY_SEP: &[char] = &[';', '&'];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum UriError {
    #[error("expected a URI scheme of 'spice://', 'spice+tls://' or 'spice+unix://'")]
    BadScheme,
    #[error("missing closing ']' in authority")]
    UnclosedBracket,
    #[error("failed to parse key in URI '{0}'")]
    BadKey(String),
    #[error("unknown key in spice URI: '{0}'")]
    UnknownKey(String),
    #[error("double set of '{0}' in URI")]
    DuplicateKey(String),
    #[error("missing port or tls-port in spice URI")]
    MissingPort,
    #[error("invalid percent-encoding in URI")]
    BadEscape,
    #[error("invalid port value '{0}'")]
    BadPort(String),
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct SpiceUri {
    /// Host as written; IPv6 literals keep their brackets (`[::1]`).
    pub host: Option<String>,
    pub port: Option<String>,
    pub tls_port: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub unix_path: Option<String>,
}

impl SpiceUri {
    pub fn parse(original: &str) -> Result<Self, UriError> {
        if let Some(path) = original.strip_prefix(SCHEME_SPICE_UNIX) {
            return Ok(Self {
                unix_path: Some(path.to_string()),
                ..Default::default()
            });
        }

        let (mut authority, tls_scheme) = if let Some(a) = original.strip_prefix(SCHEME_SPICE) {
            (a, false)
        } else if let Some(a) = original.strip_prefix(SCHEME_SPICE_TLS) {
            (a, true)
        } else {
            return Err(UriError::BadScheme);
        };

        let mut username = None;
        if let Some((user, rest)) = authority.split_once('@') {
            username = Some(unescape(user)?);
            authority = rest;
        }

        // Split off the (ignored) path, then the query from whichever came last.
        let (authority, path) = match authority.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (authority, None),
        };
        let (authority, path, query) = match path {
            Some(p) => {
                let i = p.find(QUERY_START).unwrap_or(p.len());
                (authority, Some(&p[..i]), p.get(i + 1..).unwrap_or(""))
            }
            None => {
                let i = authority.find(QUERY_START).unwrap_or(authority.len());
                (&authority[..i], None, authority.get(i + 1..).unwrap_or(""))
            }
        };

        let (host, mut port) = if let Some(inner) = authority.strip_prefix('[') {
            let (h, after) = inner.split_once(']').ok_or(UriError::UnclosedBracket)?;
            let port = after.strip_prefix(':').map(str::to_string);
            (format!("[{h}]"), port)
        } else {
            match authority.split_once(':') {
                Some((h, p)) => (unescape(h)?, Some(p.to_string())),
                None => (unescape(authority)?, None),
            }
        };

        if let Some(p) = path
            && !p.is_empty()
            && p != "/"
        {
            warn!("unexpected path data '{p}' in spice URI; ignoring it");
        }

        let mut tls_port = None;
        let mut password = None;
        let mut q = query;
        while !q.is_empty() {
            // sscanf("%31[-a-zA-Z0-9]=%n")
            let klen = q
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .unwrap_or(q.len());
            if klen == 0 || klen > 31 || q.as_bytes().get(klen) != Some(&b'=') {
                return Err(UriError::BadKey(q.to_string()));
            }
            let key = &q[..klen];
            q = &q[klen + 1..];
            if q.is_empty() {
                // key without value
                break;
            }
            if q.starts_with(QUERY_SEP) {
                // empty value: another argument follows
                q = &q[1..];
                continue;
            }
            // sscanf("%127[^;&]%n"): at most 127 bytes, then skip one separator
            let mut vlen = q.find(QUERY_SEP).unwrap_or(q.len()).min(127);
            while !q.is_char_boundary(vlen) {
                vlen -= 1;
            }
            let value = &q[..vlen];
            q = &q[vlen..];
            if let Some(c) = q.chars().next() {
                q = &q[c.len_utf8()..];
            }

            if tls_scheme && (key == "port" || key == "tls-port") {
                warn!("spice+tls:// scheme doesn't accept '{key}'");
                continue;
            }
            let target = match key {
                "port" => &mut port,
                "tls-port" => &mut tls_port,
                "password" => {
                    warn!("password may be visible in process listings");
                    &mut password
                }
                _ => return Err(UriError::UnknownKey(key.to_string())),
            };
            if target.is_some() {
                return Err(UriError::DuplicateKey(key.to_string()));
            }
            *target = Some(unescape(value)?);
        }

        if port.is_none() && tls_port.is_none() {
            return Err(UriError::MissingPort);
        }
        if tls_scheme {
            tls_port = port.take();
        }
        Ok(Self {
            host: Some(host),
            port,
            tls_port,
            username,
            password,
            unix_path: None,
        })
    }

    /// The host without IPv6 brackets, ready for name resolution.
    pub fn connect_host(&self) -> Option<&str> {
        self.host.as_deref().map(|h| {
            h.strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .unwrap_or(h)
        })
    }

    /// Parse a port string as `spice_session_channel_open_host` does.
    pub fn port_number(p: &str) -> Result<u16, UriError> {
        match p.parse::<u16>() {
            Ok(n) if n > 0 => Ok(n),
            _ => Err(UriError::BadPort(p.to_string())),
        }
    }
}

/// `spice_uri_create`; never includes the password.
impl fmt::Display for SpiceUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(p) = &self.unix_path {
            return write!(f, "{SCHEME_SPICE_UNIX}{p}");
        }
        let host = self.host.as_deref().unwrap_or("");
        match (&self.port, &self.tls_port) {
            (Some(p), Some(t)) => write!(f, "{SCHEME_SPICE}{host}?port={p}&tls-port={t}"),
            (None, Some(t)) => write!(f, "{SCHEME_SPICE_TLS}{host}:{t}"),
            (Some(p), None) => write!(f, "{SCHEME_SPICE}{host}:{p}"),
            (None, None) => write!(f, "{SCHEME_SPICE}{host}"),
        }
    }
}

impl fmt::Debug for SpiceUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpiceUri")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls_port", &self.tls_port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("unix_path", &self.unix_path)
            .finish()
    }
}

/// `g_uri_unescape_string(s, NULL)`: percent-decoding; invalid escapes and
/// escaped NULs are errors.
fn unescape(s: &str) -> Result<String, UriError> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3).ok_or(UriError::BadEscape)?;
            let v = std::str::from_utf8(hex)
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or(UriError::BadEscape)?;
            if v == 0 {
                return Err(UriError::BadEscape);
            }
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| UriError::BadEscape)
}

#[cfg(test)]
mod tests {
    //! Ported from spice-gtk `tests/session.c`.
    use super::*;

    struct Case {
        port: Option<&'static str>,
        tls_port: Option<&'static str>,
        host: Option<&'static str>,
        username: Option<&'static str>,
        password: Option<&'static str>,
        input: &'static str,
        output: Option<&'static str>,
        unix_path: Option<&'static str>,
    }

    const NONE: Case = Case {
        port: None,
        tls_port: None,
        host: None,
        username: None,
        password: None,
        input: "",
        output: None,
        unix_path: None,
    };

    fn check(cases: &[Case]) {
        for c in cases {
            let u = SpiceUri::parse(c.input).unwrap_or_else(|e| panic!("{}: {e}", c.input));
            assert_eq!(u.to_string(), c.output.unwrap_or(c.input), "{}", c.input);
            assert_eq!(u.port.as_deref(), c.port, "{}", c.input);
            assert_eq!(u.tls_port.as_deref(), c.tls_port, "{}", c.input);
            assert_eq!(u.host.as_deref(), c.host, "{}", c.input);
            assert_eq!(u.username.as_deref(), c.username, "{}", c.input);
            assert_eq!(u.password.as_deref(), c.password, "{}", c.input);
            assert_eq!(u.unix_path.as_deref(), c.unix_path, "{}", c.input);

            // Second half of test_session_uri_good: build from parts, check URI.
            let built = SpiceUri {
                host: c.host.map(Into::into),
                port: c.port.map(Into::into),
                tls_port: c.tls_port.map(Into::into),
                username: c.username.map(Into::into),
                password: c.password.map(Into::into),
                unix_path: c.unix_path.map(Into::into),
            };
            assert_eq!(built.to_string(), c.output.unwrap_or(c.input));
        }
    }

    #[test]
    fn bad_uris() {
        use UriError::*;
        let cases = [
            ("scheme://host?port", BadScheme),
            ("spice://[ipv6-host:42", UnclosedBracket),
            ("spice://host??", BadKey("?".into())),
            (
                "spice://host:5900?unknown=value",
                UnknownKey("unknown".into()),
            ),
            ("spice://hostname", MissingPort),
            (
                "spice://hostname?port=1234&port=3456",
                DuplicateKey("port".into()),
            ),
            (
                "spice://hostname?tls-port=1234&port=3456&tls-port=5678",
                DuplicateKey("tls-port".into()),
            ),
            (
                "spice://hostname:5900?tls-port=1234&port=3456",
                DuplicateKey("port".into()),
            ),
            ("spice+tls://hostname?tls-port=1234&port=3456", MissingPort),
        ];
        for (uri, err) in cases {
            assert_eq!(SpiceUri::parse(uri), Err(err), "{uri}");
        }
    }

    #[test]
    fn ipv4_good() {
        check(&[
            Case {
                port: Some("5900"),
                host: Some("localhost"),
                input: "spice://localhost?port=5900&tls-port=",
                output: Some("spice://localhost:5900"),
                ..NONE
            },
            Case {
                port: Some("5910"),
                host: Some("localhost"),
                username: Some("user"),
                input: "spice://user@localhost?tls-port=&port=5910",
                output: Some("spice://localhost:5910"),
                ..NONE
            },
            Case {
                tls_port: Some("5920"),
                host: Some("localhost"),
                username: Some("user"),
                password: Some("password"),
                input: "spice://user@localhost?tls-port=5920&port=&password=password",
                output: Some("spice+tls://localhost:5920"),
                ..NONE
            },
            Case {
                tls_port: Some("5930"),
                host: Some("localhost"),
                input: "spice://localhost?port=&tls-port=5930",
                output: Some("spice+tls://localhost:5930"),
                ..NONE
            },
            Case {
                port: Some("42"),
                host: Some("localhost"),
                input: "spice://localhost:42",
                ..NONE
            },
            Case {
                port: Some("42"),
                tls_port: Some("5930"),
                host: Some("localhost"),
                input: "spice://localhost:42?tls-port=5930",
                output: Some("spice://localhost?port=42&tls-port=5930"),
                ..NONE
            },
            Case {
                port: Some("42"),
                tls_port: Some("5930"),
                host: Some("127.0.0.1"),
                input: "spice://127.0.0.1:42?tls-port=5930",
                output: Some("spice://127.0.0.1?port=42&tls-port=5930"),
                ..NONE
            },
            Case {
                input: "spice+tls://hostname:39",
                host: Some("hostname"),
                tls_port: Some("39"),
                ..NONE
            },
        ]);
    }

    #[test]
    fn ipv6_good() {
        check(&[
            Case {
                port: Some("5900"),
                host: Some("[2010:836B:4179::836B:4179]"),
                input: "spice://[2010:836B:4179::836B:4179]?port=5900&tls-port=",
                output: Some("spice://[2010:836B:4179::836B:4179]:5900"),
                ..NONE
            },
            Case {
                port: Some("5910"),
                host: Some("[::192.9.5.5]"),
                username: Some("user"),
                input: "spice://user@[::192.9.5.5]?tls-port=&port=5910",
                output: Some("spice://[::192.9.5.5]:5910"),
                ..NONE
            },
            Case {
                tls_port: Some("5920"),
                host: Some("[3ffe:2a00:100:7031::1]"),
                username: Some("user"),
                password: Some("password"),
                input: "spice://user@[3ffe:2a00:100:7031::1]?tls-port=5920&port=&password=password",
                output: Some("spice+tls://[3ffe:2a00:100:7031::1]:5920"),
                ..NONE
            },
            Case {
                tls_port: Some("5930"),
                host: Some("[1080:0:0:0:8:800:200C:417A]"),
                input: "spice://[1080:0:0:0:8:800:200C:417A]?port=&tls-port=5930",
                output: Some("spice+tls://[1080:0:0:0:8:800:200C:417A]:5930"),
                ..NONE
            },
            Case {
                port: Some("42"),
                host: Some("[FEDC:BA98:7654:3210:FEDC:BA98:7654:3210]"),
                input: "spice://[FEDC:BA98:7654:3210:FEDC:BA98:7654:3210]:42",
                ..NONE
            },
            Case {
                port: Some("42"),
                tls_port: Some("5930"),
                host: Some("[::192.9.5.5]"),
                input: "spice://[::192.9.5.5]:42?tls-port=5930",
                output: Some("spice://[::192.9.5.5]?port=42&tls-port=5930"),
                ..NONE
            },
            Case {
                port: Some("42"),
                tls_port: Some("5930"),
                host: Some("[::FFFF:129.144.52.38]"),
                input: "spice://[::FFFF:129.144.52.38]:42?tls-port=5930",
                output: Some("spice://[::FFFF:129.144.52.38]?port=42&tls-port=5930"),
                ..NONE
            },
        ]);
    }

    #[test]
    fn unix_good() {
        check(&[
            Case {
                input: "spice+unix:///tmp/foo.sock",
                unix_path: Some("/tmp/foo.sock"),
                ..NONE
            },
            Case {
                input: "spice+unix://",
                unix_path: Some(""),
                ..NONE
            },
            Case {
                input: "spice+unix:///tmp/foo.sock?password=frobnicate",
                unix_path: Some("/tmp/foo.sock?password=frobnicate"),
                ..NONE
            },
        ]);
    }

    #[test]
    fn misc() {
        let u = SpiceUri::parse("spice://h%41st:1/?password=a%20b;tls-port=2").unwrap();
        assert_eq!(u.host.as_deref(), Some("hAst"));
        assert_eq!(u.password.as_deref(), Some("a b"));
        assert_eq!(u.tls_port.as_deref(), Some("2"));
        assert!(!format!("{u:?}").contains("a b"));
        assert!(!u.to_string().contains("a b"));
        assert_eq!(u.connect_host(), Some("hAst"));
        let u = SpiceUri::parse("spice://[::1]:5900").unwrap();
        assert_eq!(u.connect_host(), Some("::1"));
        assert_eq!(SpiceUri::port_number("5900"), Ok(5900));
        assert!(SpiceUri::port_number("0").is_err());
        assert!(SpiceUri::port_number("70000").is_err());
        assert!(SpiceUri::port_number("").is_err());
        assert!(SpiceUri::parse("spice://h:1?port").is_err());
        assert!(SpiceUri::parse("spice://h:1?=3").is_err());
        assert_eq!(
            SpiceUri::parse("spice://h:1?password=").unwrap().password,
            None
        );
    }
}

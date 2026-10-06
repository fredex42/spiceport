//! Clipboard newline conversion (`spice_dos2unix` / `spice_unix2dos` in
//! spice-gtk `spice-util.c`).

/// CRLF → LF. A trailing NUL is dropped, as spice-gtk does.
pub fn dos2unix(s: &str) -> String {
    let s = s.strip_suffix('\0').unwrap_or(s);
    s.replace("\r\n", "\n")
}

/// LF → CRLF, without doubling an existing `\r` before the `\n`.
pub fn unix2dos(s: &str) -> String {
    let s = s.strip_suffix('\0').unwrap_or(s);
    let mut out = String::with_capacity(s.len() * 2);
    for line in s.split_inclusive('\n') {
        match line.strip_suffix('\n') {
            Some(body) => {
                out.push_str(body);
                if !out.ends_with('\r') {
                    out.push('\r');
                }
                out.push('\n');
            }
            None => out.push_str(line),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    //! Ported from spice-gtk tests/util.c.
    use super::*;

    const DOS2UNIX: u8 = 1;
    const UNIX2DOS: u8 = 2;
    const CASES: &[(&str, &str, u8)] = &[
        ("", "", DOS2UNIX | UNIX2DOS),
        ("a", "a", DOS2UNIX | UNIX2DOS),
        ("\r\n", "\n", DOS2UNIX | UNIX2DOS),
        ("\r\n\r\n", "\n\n", DOS2UNIX | UNIX2DOS),
        ("a\r\n", "a\n", DOS2UNIX | UNIX2DOS),
        ("a\r\n\r\n", "a\n\n", DOS2UNIX | UNIX2DOS),
        ("\r\n\r\na\r\n\r\n", "\n\na\n\n", DOS2UNIX | UNIX2DOS),
        ("1\r\n\r\na\r\n\r\n2", "1\n\na\n\n2", DOS2UNIX | UNIX2DOS),
        ("\n", "\n", DOS2UNIX),
        ("\n\n", "\n\n", DOS2UNIX),
        ("\r\n", "\r\n", UNIX2DOS),
        ("\r\r\n", "\r\r\n", UNIX2DOS),
        ("é\r\né", "é\né", DOS2UNIX | UNIX2DOS),
        ("\r\né\r\né\r\n", "\né\né\n", DOS2UNIX | UNIX2DOS),
    ];

    #[test]
    fn dos_unix() {
        for &(d, u, flags) in CASES {
            if flags & DOS2UNIX != 0 {
                assert_eq!(dos2unix(d), u, "dos2unix {d:?}");
                assert_eq!(dos2unix(&format!("{d}\0")), u, "dos2unix {d:?} with NUL");
            }
            if flags & UNIX2DOS != 0 {
                assert_eq!(unix2dos(u), d, "unix2dos {u:?}");
                assert_eq!(unix2dos(&format!("{u}\0")), d, "unix2dos {u:?} with NUL");
            }
        }
    }
}

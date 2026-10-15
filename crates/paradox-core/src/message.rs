use similar::{ChangeTag, TextDiff};
use std::path::Path;

/// Return only the permitted message line. A normal file never reads line two.
pub fn message_line(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or(bytes.len());
    let first = bytes[..end].strip_suffix(b"\r").unwrap_or(&bytes[..end]);
    if first.starts_with(b"#!") || first == b"<?php" {
        let rest = bytes.get(end + 1..).unwrap_or_default();
        let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
        rest[..end].strip_suffix(b"\r").unwrap_or(&rest[..end])
    } else {
        first
    }
}

pub fn label(path: &Path, current: &[u8], previous: &[u8]) -> (String, bool) {
    let line = message_line(current);
    if path.extension().is_none_or(|e| e != "json") && line != message_line(previous) {
        for prefix in [b"//>".as_slice(), b"#>", b"-->", b";>", b"%>"] {
            if let Some(value) = line
                .strip_prefix(prefix)
                .and_then(|v| std::str::from_utf8(v).ok())
            {
                let value = value.trim_start();
                if !value.is_empty() {
                    return (value.into(), false);
                }
            }
        }
    }
    let old = String::from_utf8_lossy(previous);
    let new = String::from_utf8_lossy(current);
    let diff = TextDiff::from_lines(old.as_ref(), new.as_ref());
    let (mut added, mut removed) = (0, 0);
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    (format!("+{added} -{removed}"), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn permitted_lines_and_stale_messages() {
        for prefix in ["//>", "#>", "-->", ";>", "%>"] {
            let text = format!("{prefix}   fix ü\r\nbody\n");
            assert_eq!(
                label(Path::new("file.ts"), text.as_bytes(), b"old"),
                ("fix ü".into(), false)
            );
            let changed = format!("{prefix}   fix ü\r\nchanged\n");
            assert!(label(Path::new("file.ts"), changed.as_bytes(), text.as_bytes()).1);
        }
        for first in ["#!/bin/sh", "<?php"] {
            assert_eq!(
                message_line(format!("{first}\r\n#> second\r\n#> third").as_bytes()),
                b"#> second"
            );
        }
        assert!(label(Path::new("file.json"), b"//> no", b"").1);
        assert_eq!(message_line(b""), b"");
        assert_eq!(message_line(b"#!/bin/sh"), b"");
        assert_eq!(message_line(b"normal\n//> ignored"), b"normal");
        assert_eq!(message_line(&vec![b'x'; 1_000_000]).len(), 1_000_000);
    }

    proptest! {
        #[test]
        fn parser_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..10000)) {
            let _ = message_line(&bytes);
        }
        #[test]
        fn later_lines_cannot_supply_messages(first in "[^\r\n]{0,300}", tail in ".*") {
            prop_assume!(!first.starts_with("#!") && first != "<?php");
            let bytes = format!("{first}\n{tail}");
            prop_assert_eq!(message_line(bytes.as_bytes()), first.as_bytes());
        }
    }
}

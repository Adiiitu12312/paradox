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

/// Shared grammar for capture and the staged-content stripper added in M4.
/// A closing delimiter must end the permitted line; executable tails are not markers.
pub fn marker<'a>(path: &Path, bytes: &'a [u8]) -> Option<&'a str> {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        return None;
    }
    let line = std::str::from_utf8(message_line(bytes)).ok()?;
    for (opening, closing) in [("/*>", "*/"), ("<!-->", "-->")] {
        if let Some(rest) = line.strip_prefix(opening) {
            let value = rest.trim_end().strip_suffix(closing)?.trim();
            return (!value.contains(closing)).then_some(value);
        }
    }
    let value = if let Some(rest) = line.strip_prefix("--") {
        rest.trim_start().strip_prefix('>')?
    } else {
        ["//>", "#>", ";>", "%>"]
            .iter()
            .find_map(|p| line.strip_prefix(p))?
    }
    .trim_start();
    Some(value)
}

pub fn label(path: &Path, current: &[u8], previous: &[u8]) -> (String, bool) {
    if message_line(current) != message_line(previous)
        && let Some(value) = marker(path, current)
        && !value.is_empty()
    {
        return (value.into(), false);
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

/// Byte range of the complete permitted marker line, including its newline.
pub fn marker_range(path: &Path, bytes: &[u8]) -> Option<std::ops::Range<usize>> {
    marker(path, bytes)?;
    let first_end = bytes.iter().position(|b| *b == b'\n').map_or(bytes.len(), |i| i + 1);
    let first = bytes[..first_end].strip_suffix(b"\n").unwrap_or(&bytes[..first_end]);
    let first = first.strip_suffix(b"\r").unwrap_or(first);
    let start = if first.starts_with(b"#!") || first == b"<?php" { first_end } else { 0 };
    let end = bytes[start..].iter().position(|b| *b == b'\n').map_or(bytes.len(), |i| start + i + 1);
    Some(start..end)
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

    #[test]
    fn closed_markers_and_spaced_sql_preserve_message_boundaries() {
        for (path, line) in [
            ("file.css", "/*> fix ü */"),
            ("file.scss", "/*> fix ü */"),
            ("file.less", "/*> fix ü */"),
            ("file.html", "<!--> fix ü -->"),
            ("file.md", "<!--> fix ü -->"),
            ("file.vue", "<!--> fix ü -->"),
            ("file.svelte", "<!--> fix ü -->"),
            ("file.xml", "<!--> fix ü -->"),
            ("file.sql", "-- > fix ü"),
            ("file.sql", "--> fix ü"),
        ] {
            let bytes = format!("{line}\r\nbody\r\n");
            assert_eq!(marker(Path::new(path), bytes.as_bytes()), Some("fix ü"));
            assert_eq!(
                label(Path::new(path), bytes.as_bytes(), b"old"),
                ("fix ü".into(), false)
            );
            let next = format!("{line}\r\nchanged\r\n");
            assert!(label(Path::new(path), next.as_bytes(), bytes.as_bytes()).1);
            for first in ["#!/bin/sh", "<?php"] {
                let bytes = format!("{first}\r\n{line}\r\nthird\n");
                assert_eq!(marker(Path::new(path), bytes.as_bytes()), Some("fix ü"));
            }
        }
        for line in [
            "/*> missing",
            "<!--> missing",
            "/*> msg */ code()",
            "<!--> msg --> <code>",
            "/*> one */ two */",
        ] {
            assert_eq!(marker(Path::new("file.css"), line.as_bytes()), None);
        }
        assert_eq!(marker(Path::new("file.css"), b"/*> */"), Some(""));
        assert!(label(Path::new("file.css"), b"/*> */", b"old").1);
        assert_eq!(marker(Path::new("file.JSON"), b"/*> no */"), None);
        assert_eq!(
            marker(Path::new("file.html"), b"ordinary\n<!--> ignored -->"),
            None
        );
    }

    proptest! {
        #[test]
        fn parser_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..10000)) {
            let _ = message_line(&bytes);
            let _ = marker(Path::new("file.css"), &bytes);
            let _ = marker(Path::new("file.html"), &bytes);
        }
        #[test]
        fn all_marker_forms_read_only_permitted_lines(message in "[a-zA-Z0-9 ü]{0,200}", tail in ".*") {
            for (opening,closing) in [("//>",""),("#>",""),("-->",""),("-- >",""),(";>",""),("%>",""),("/*>","*/"),("<!-->","-->")] {
                let line = format!("{opening} {message} {closing}");
                let one = format!("{line}\r\n{tail}");
                let two = format!("#!/bin/sh\r\n{line}\r\n{tail}");
                prop_assert_eq!(marker(Path::new("file.txt"),one.as_bytes()),marker(Path::new("file.txt"),line.as_bytes()));
                prop_assert_eq!(marker(Path::new("file.txt"),two.as_bytes()),marker(Path::new("file.txt"),line.as_bytes()));
            }
        }
        #[test]
        fn later_lines_cannot_supply_messages(first in "[^\r\n]{0,300}", tail in ".*") {
            prop_assume!(!first.starts_with("#!") && first != "<?php");
            let bytes = format!("{first}\n{tail}");
            prop_assert_eq!(message_line(bytes.as_bytes()), first.as_bytes());
        }
    }
}

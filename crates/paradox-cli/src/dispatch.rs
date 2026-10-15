use std::ffi::OsString;

pub const OWNED: &[&str] = &[
    "commit", "init", "ui", "git", "loop", "loops", "compare", "rewind", "timeline", "pin",
    "unpin", "track", "untrack",
];

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Git,
    Help,
    ForceGit { index: usize },
    Owned { index: usize },
    FileCommit { index: usize, file: usize },
}

/// Inspect only ASCII option syntax. Never rebuild or decode forwarded arguments.
/// Unknown/incomplete flags belong to Git, including future Git global flags.
pub fn route(args: &[OsString]) -> Route {
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        return Route::Help;
    }
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let bytes = arg.as_encoded_bytes();
        let Some(s) = arg.to_str() else { return Route::Git };
        if matches!(s, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env") {
            if args.get(i + 1).is_none() { return Route::Git; }
            i += 2;
            continue;
        }
        if [b"-C".as_slice(), b"-c"].iter().any(|p| bytes.starts_with(p) && bytes.len() > p.len())
            || ["--git-dir=", "--work-tree=", "--namespace=", "--config-env=", "--exec-path="]
                .iter().any(|p| s.starts_with(p))
            || matches!(s, "--no-pager" | "-P" | "-p" | "--paginate" | "--bare"
                | "--no-replace-objects" | "--no-lazy-fetch" | "--no-optional-locks" | "--no-advice")
        {
            i += 1;
            continue;
        }
        if s.starts_with('-') { return Route::Git; }
        if s == "git" { return Route::ForceGit { index: i }; }
        if OWNED.contains(&s) {
            // A help request must never trigger auto-staging or setup.
            if matches!(s, "commit" | "init") && args[i + 1..].iter()
                .any(|a| a == "-h" || a == "--help") { return Route::Git; }
            return Route::Owned { index: i };
        }
        // Explicit path syntax avoids interpreting `show commit` or `log commit`
        // as the alternate file-first convenience. Implementation arrives in M4.
        if (s.contains('.') || s.contains('/') || s.contains('\\'))
            && args.get(i + 1).is_some_and(|a| a == "commit")
        {
            return Route::FileCommit { index: i + 1, file: i };
        }
        return Route::Git;
    }
    Route::Git
}

/// Remove just Paradox's `git` escape token, retaining all global Git options.
pub fn force_git_args(args: &[OsString], index: usize) -> Vec<OsString> {
    args[..index].iter().chain(&args[index + 1..]).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn routing_table() {
        use Route::*;
        let cases: &[(&[&str], Route)] = &[
            (&[], Git), (&["--help"], Help), (&["-h"], Help),
            (&["--version"], Git), (&["-v"], Git),
            (&["--exec-path", "commit"], Git), (&["--html-path"], Git),
            (&["--help", "commit"], Git), (&["commit", "-h"], Git),
            (&["--future-option", "loop"], Git), (&["-C"], Git),
            (&["status"], Git), (&["diff", "loop"], Git), (&["show", "commit"], Git),
            (&["log"], Git), (&["gc"], Git), (&["restore", "file"], Git),
            (&["rebase", "-i"], Git), (&["-C", "other", "status"], Git),
            (&["commit", "-m", "x"], Owned { index: 0 }),
            (&["-c", "user.name=x", "commit"], Owned { index: 2 }),
            (&["-Cother", "-cuser.name=x", "loop"], Owned { index: 2 }),
            (&["-C", "one", "-C", "two", "--no-pager", "loops"], Owned { index: 5 }),
            (&["--git-dir=x", "--work-tree", "y", "loop"], Owned { index: 3 }),
            (&["--namespace", "x", "--config-env=a=b", "pin"], Owned { index: 3 }),
            (&["--exec-path=x", "loop"], Owned { index: 1 }),
            (&["git", "commit"], ForceGit { index: 0 }),
            (&["-C", "other", "git", "init"], ForceGit { index: 2 }),
            (&["a.ts", "commit", "-m", "x"], FileCommit { index: 1, file: 0 }),
        ];
        for (args, expected) in cases {
            let args: Vec<_> = args.iter().map(OsString::from).collect();
            assert_eq!(&route(&args), expected, "{args:?}");
        }
    }

    proptest! {
        #[test]
        fn routing_never_mutates_arguments(args in prop::collection::vec(".*", 0..40)) {
            let args: Vec<_> = args.into_iter().map(OsString::from).collect();
            let original = args.clone();
            let _ = route(&args);
            prop_assert_eq!(&args, &original);
            let mut escaped = vec![OsString::from("git")];
            escaped.extend(args.clone());
            prop_assert_eq!(route(&escaped), Route::ForceGit { index: 0 });
            prop_assert_eq!(force_git_args(&escaped, 0), original);
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments_are_preserved() {
        use std::os::unix::ffi::OsStringExt;
        let args = vec![OsString::from("git"), OsString::from_vec(vec![0xff, 0xfe])];
        assert_eq!(force_git_args(&args, 0), args[1..]);
        assert_eq!(route(&args[1..]), Route::Git);
    }
}

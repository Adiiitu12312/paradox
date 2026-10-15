use std::ffi::OsString;

pub const OWNED: &[&str] = &[
    "commit", "init", "ui", "git", "loop", "loops", "compare", "rewind", "timeline", "pin",
    "unpin", "track", "untrack", "watch",
];

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Git,
    Help,
    ForceGit { index: usize },
    Owned { index: usize },
    FileCommit { index: usize, file: usize },
}

enum Scan {
    Command(usize),
    Delimiter(usize),
    Unknown,
}

/// Shared raw global-option scan. An unknown flag cannot safely identify a
/// future Git subcommand, so it always stays untouched on the pass-through path.
fn scan(args: &[OsString]) -> Scan {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.to_str() else {
            return Scan::Command(i);
        };
        if text == "--" {
            return Scan::Delimiter(i);
        }
        if matches!(
            text,
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
        ) {
            if args.get(i + 1).is_none() {
                return Scan::Unknown;
            }
            i += 2;
            continue;
        }
        if [
            "-C",
            "-c",
            "--git-dir=",
            "--work-tree=",
            "--namespace=",
            "--config-env=",
            "--exec-path=",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix) && text.len() > prefix.len())
            || matches!(
                text,
                "--no-pager"
                    | "-P"
                    | "-p"
                    | "--paginate"
                    | "--bare"
                    | "--no-replace-objects"
                    | "--no-lazy-fetch"
                    | "--no-optional-locks"
                    | "--no-advice"
            )
        {
            i += 1;
            continue;
        }
        if text.starts_with('-') {
            return Scan::Unknown;
        }
        return Scan::Command(i);
    }
    Scan::Unknown
}

/// The actual subcommand following recognized native Git global arguments.
/// No lossy decoding or argument reconstruction occurs here.
pub fn git_subcommand_index(args: &[OsString]) -> Option<usize> {
    match scan(args) {
        Scan::Command(index) => Some(index),
        _ => None,
    }
}

/// A Paradox file escape delimiter is routing syntax, never a Git global flag.
pub fn global_args(args: &[OsString], index: usize) -> &[OsString] {
    let end = match scan(args) {
        Scan::Delimiter(delimiter) if delimiter + 1 == index => delimiter,
        _ => index,
    };
    &args[..end]
}

/// Inspect raw argv before constructing Clap, a database, or configuration.
pub fn route(args: &[OsString]) -> Route {
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        return Route::Help;
    }
    let index = match scan(args) {
        Scan::Command(index) => index,
        Scan::Delimiter(index) => {
            return if args.get(index + 1).is_some()
                && args.get(index + 2).is_some_and(|arg| arg == "commit")
            {
                Route::FileCommit {
                    index: index + 2,
                    file: index + 1,
                }
            } else {
                Route::Git
            };
        }
        Scan::Unknown => return Route::Git,
    };
    let Some(command) = args[index].to_str() else {
        return Route::Git;
    };
    if command == "git" {
        return Route::ForceGit { index };
    }
    if OWNED.contains(&command) {
        if matches!(command, "commit" | "init")
            && args[index + 1..]
                .iter()
                .any(|arg| arg == "-h" || arg == "--help")
        {
            return Route::Git;
        }
        return Route::Owned { index };
    }
    if (command.contains('.') || command.contains('/') || command.contains('\\'))
        && args.get(index + 1).is_some_and(|arg| arg == "commit")
    {
        return Route::FileCommit {
            index: index + 1,
            file: index,
        };
    }
    Route::Git
}

/// Remove just Paradox's `git` escape token, retaining all global Git options.
pub fn force_git_args(args: &[OsString], index: usize) -> Vec<OsString> {
    args[..index]
        .iter()
        .chain(&args[index + 1..])
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn routing_table() {
        use Route::*;
        let cases: &[(&[&str], Route)] = &[
            (&[], Git),
            (&["--help"], Help),
            (&["-h"], Help),
            (&["--version"], Git),
            (&["-v"], Git),
            (&["--exec-path", "commit"], Git),
            (&["--html-path"], Git),
            (&["--help", "commit"], Git),
            (&["commit", "-h"], Git),
            (&["--future-option", "loop"], Git),
            (
                &["--", "commit", "commit", "-m", "x"],
                FileCommit { index: 2, file: 1 },
            ),
            (
                &["-C", "repo", "--", "ui", "commit"],
                FileCommit { index: 4, file: 3 },
            ),
            (
                &["--", "-strange", "commit"],
                FileCommit { index: 2, file: 1 },
            ),
            (&["--", "not-a-command"], Git),
            (&["-C"], Git),
            (&["status"], Git),
            (&["diff", "loop"], Git),
            (&["show", "commit"], Git),
            (&["log"], Git),
            (&["gc"], Git),
            (&["restore", "file"], Git),
            (&["rebase", "-i"], Git),
            (&["-C", "other", "status"], Git),
            (&["commit", "-m", "x"], Owned { index: 0 }),
            (&["-c", "user.name=x", "commit"], Owned { index: 2 }),
            (&["-Cother", "-cuser.name=x", "loop"], Owned { index: 2 }),
            (
                &["-C", "one", "-C", "two", "--no-pager", "loops"],
                Owned { index: 5 },
            ),
            (
                &["--git-dir=x", "--work-tree", "y", "loop"],
                Owned { index: 3 },
            ),
            (
                &["--namespace", "x", "--config-env=a=b", "pin"],
                Owned { index: 3 },
            ),
            (&["--exec-path=x", "loop"], Owned { index: 1 }),
            (&["git", "commit"], ForceGit { index: 0 }),
            (&["-C", "other", "git", "init"], ForceGit { index: 2 }),
            (
                &["a.ts", "commit", "-m", "x"],
                FileCommit { index: 1, file: 0 },
            ),
        ];
        for (args, expected) in cases {
            let args: Vec<_> = args.iter().map(OsString::from).collect();
            assert_eq!(&route(&args), expected, "{args:?}");
        }
    }

    #[test]
    fn actual_subcommand_and_file_escape_globals_are_unambiguous() {
        let args: Vec<_> = ["-C", "repo", "-c", "alias.other=reset", "reset", "--hard"]
            .map(OsString::from)
            .to_vec();
        assert_eq!(git_subcommand_index(&args), Some(4));
        assert_eq!(global_args(&args, 4), &args[..4]);
        let escaped: Vec<_> = ["-C", "repo", "--", "commit", "commit", "-m", "message"]
            .map(OsString::from)
            .to_vec();
        assert_eq!(git_subcommand_index(&escaped), None);
        assert_eq!(global_args(&escaped, 3), &escaped[..2]);
        let unknown: Vec<_> = ["--future-global", "reset"].map(OsString::from).to_vec();
        assert_eq!(git_subcommand_index(&unknown), None);
        let value: Vec<_> = ["-c", "--", "status"].map(OsString::from).to_vec();
        assert_eq!(global_args(&value, 2), &value[..2]);
        for owned in OWNED {
            let args = vec![OsString::from(owned), OsString::from("commit")];
            assert!(matches!(
                route(&args),
                Route::Owned { index: 0 } | Route::ForceGit { index: 0 }
            ));
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

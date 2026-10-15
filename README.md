# Paradox

A Rust Git wrapper that will add local per-file loops and a browser Git UI.
Git remains the source of truth; every Git operation uses the installed Git binary.

**Development status: milestone M1.** This build implements the workspace and
dispatcher. Loop storage, automatic capture, auto-staging commits and the UI are
scheduled for later milestones. Do not alias `git` to this development build.

## Build on GitHub (preferred for this project)

Local editing needs no Rust, C++ tools or extra Node installation. Push this
project to GitHub and open **Actions → Build and test Paradox**. The workflow
builds and tests on GitHub's Windows, Linux and macOS machines. After a successful
run, download the matching `paradox-Windows-X64`, `paradox-Linux-X64` or macOS
artifact from that run. Extract it and run the binary locally with Git on PATH.
The macOS runner architecture is shown in the artifact name.

Artifacts expire after seven days. The workflow supports manual runs as well as
pushes and pull requests. GitHub Actions availability and billing depend on your
repository and account; no paid build service has been enabled by this project.

## Optional local build

For contributors who choose to compile locally, install Git and Rust stable with
rustfmt and clippy. On Windows, install Visual
Studio C++ Build Tools with the Windows SDK. Node.js LTS is required when frontend
work starts in M6. No JavaScript code is built in M1.

```text
cargo build --release
```

The binary is `target/release/paradox` (`paradox.exe` on Windows). It requires the
real Git executable on PATH. Shell aliases named `git` do not affect subprocess
resolution. Do not replace the Git executable with Paradox or create a `git`
executable shim pointing back to Paradox.

```text
paradox status
paradox log --graph --oneline
paradox -C /path/to/repo diff
paradox git commit -m "use plain Git"
paradox --help
```

## M1 behavior

Pass-through commands preserve the original operating-system arguments and inherit
stdin/stdout/stderr, environment and terminal handles. They do no repository scan
and open no loop store. Unix uses process replacement. Windows starts Git in the
same console process group, waits, and returns its exit code; its console handler
lets Git handle Ctrl+C and Ctrl+Break. Git controls colors, pagers and editors.

The global-option routing follows the [Git command reference](https://git-scm.com/docs/git).
Unknown and incomplete global options are delegated to Git for validation.
`paradox --version` prints Git's version; `paradox --help` and `paradox -h` describe
Paradox. `paradox git <args>` removes only the `git` escape token and preserves
preceding global options.

These reserved commands return an explicit unavailable error in M1: `commit`,
`init`, `ui`, `loop`, `loops`, `compare`, `rewind`, `timeline`, `pin`, `unpin`,
`track`, `untrack`. They do not fall back to Git and imply that a safety feature
has run. Use `paradox git commit` and `paradox git init` for pure Git until M4.
Help requests for Git's `commit` and `init` still go to Git without doing setup.
File-first commit routing currently recognizes explicit path syntax (a dot or
path separator); its execution is reserved for M4.

## Verification

All tests that execute Paradox create OS temporary repositories **outside** this
project. They never invoke Paradox on this project or an existing user repository.
Git fixtures disable global/system config and existing hooks, use fixed test-only
identities and dates, and never contact remotes.

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
cargo test --release --test performance --locked -- --ignored --nocapture
cargo audit
```

The release performance test alternates 101 pairs after warmup and fails if the
difference between median Git and Paradox times reaches 5 ms. It measures both
`--version` and status in a temporary repo. Timing varies with machine load.

CI is configured for Windows, macOS and Linux. A local Windows test run is not
evidence that those remote jobs have run. Terminal interaction and signal delivery
also need platform-specific verification; captured output tests alone cannot
establish all interactive behavior.

## Milestone order

M1 workspace and dispatcher; M2 loop store and capture; M3 compare and rewind;
M4 automatic capture, setup and commit conveniences; M5 file picker;
M6–M8 browser UI; M9 hardening and release documentation.

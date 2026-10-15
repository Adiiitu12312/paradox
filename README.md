# Paradox

A Rust Git wrapper that will add local per-file loops and a browser Git UI.
Git remains the source of truth; every Git operation uses the installed Git binary.

**Development status: M1 accepted; M2 loop storage and capture are in progress.**
See the [M1 verification report](docs/milestones/M1.md) for measured results.
This build implements the workspace and
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

Development artifacts are uploaded after functional and terminal checks, before
the performance gate. An artifact can therefore exist on a run whose timing gate
failed; it does not mean the milestone's performance target passed.

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

On Windows use `cargo build --release --target x86_64-pc-windows-msvc`.
The binary is then under `target/x86_64-pc-windows-msvc/release/`. Explicit target
selection keeps host proc macros separate from the static Windows CRT build.
Release builds use fat LTO, one codegen unit, abort-on-panic and stripped symbols.

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

Goal G3 in the [project brief](docs/PROJECT_BRIEF.md) requires under 5 ms median
pass-through overhead on Linux/macOS and under 20 ms on Windows. Unix replaces
the process with `exec`; Windows has no `exec` and must spawn Git and wait to
preserve its terminal behavior and exit code. The Windows exception was accepted
on October 15, 2026; loop capture performance targets are unchanged.

The release test measures 101 alternating pairs after warmup for both version and
status, on all three OSes. [Stored baselines](benchmarks/passthrough.csv) also catch
regressions: the ceiling is the smaller of the OS limit and the recorded baseline
plus 50% and 1 ms scheduling allowance. Point observations are recorded separately;
the allowance avoids treating ordinary shared-runner jitter as a code regression.

CI is configured for Windows, macOS and Linux. A local Windows test run is not
evidence that those remote jobs have run. Terminal interaction and signal delivery
also need platform-specific verification; captured output tests alone cannot
establish all interactive behavior.

To measure a downloaded binary on a machine with Node.js already available,
without a compiler, run:

```text
node scripts/benchmark_host.mjs /absolute/path/to/paradox
```

This script also uses a new temporary repository outside the project and removes
that repository on exit. It enforces the same stored per-OS ceiling.

## Milestone order

M1 workspace and dispatcher; M2 loop store and capture; M3 compare and rewind;
M4 automatic capture, setup and commit conveniences; M4.5 manual watch sessions;
M5 file picker;
M6–M8 browser UI; M9 hardening and release documentation.

The [M4.5 watcher](docs/milestones/M4.5-watch.md) covers agents/tools with no edit
hook. Start it explicitly with `paradox watch` or `paradox watch --background`;
stop it with Ctrl+C or `paradox watch --stop`, and inspect it with `--status`.
These commands arrive in M4.5. It is never automatic and never installed as a
service. Idle sessions stop after 30 minutes by default. Linux users will get an
explicit coverage warning and instructions if the inotify watch limit is too low.

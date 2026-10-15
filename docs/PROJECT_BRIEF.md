# PROJECT BRIEF: PARADOX

I am not a Rust developer. You write, build, test, and debug everything. Work autonomously, fix errors without asking me, and stop only at milestone boundaries (or if a product decision genuinely blocks you). This tool will sit on top of my real source history, so correctness and data safety outrank speed and features.

## 1. WHAT WE ARE BUILDING

Paradox is a single Rust binary, `paradox`, that is a drop-in replacement for the `git` command with three additions:
1. Loops: a cheap, local, automatic safety net that records the state of individual files between Git commits, so a single file can be rewound to "5 minutes ago" without touching anything else. A "loop" is one saved version of one file at one moment, with a message, a time, and the Git commit it was made on.
2. A few Git-workflow conveniences (`paradox commit` auto-stages).
3. A full Git-level graphical UI in the browser (`paradox ui`) that also shows the per-file loop timelines.

Git remains the source of truth for history. Paradox never replaces, forks, patches, or reimplements Git; it shells out to the real `git` binary for every Git operation.

Naming note: the user-facing term is "loop" everywhere (CLI, UI, docs, error messages). Do not use the words "checkpoint", "snapshot" or "echo" in user-facing text. Internally, code may use `Loop`, `LoopStore`, etc.

## 2. THE PROBLEM

- AI coding tools make many rapid edits. Sometimes one edit breaks a file that worked 5 minutes ago. Git only records what I explicitly commit, so there is nothing to roll back to between commits. Undoing by hand or by Git means either losing good work in other files or reconstructing from memory.
- Rolling back the whole repo is too coarse. Typically one file (e.g. the backend) was broken while others (e.g. the UI) are correct and should stay.
- Linear, repo-wide histories become a mess when 5-6 files change in the same minute. History has to be per file.
- The repo is a large monorepo (about 5-6 GB of Git history, growing roughly 200-300 MB/day, 70-80 commits/day from me, my co-founder, and AI agents). Any new tool must add near-zero overhead to normal Git usage and must not add its own unbounded storage growth.
- I will not run an auto-started or always-on background process. Capture uses save/edit hooks, fallback scans, or a session-only watcher started explicitly for tools without post-edit hooks. Nothing starts a watcher automatically.

## 3. GOALS AND OUTCOMES (what "done" means)

G1. Git parity: I can `alias git=paradox` and notice nothing different for every Git command except the documented exceptions (`paradox commit` auto-stages; `paradox init` also sets up Paradox).
G2. Per-file safety net: any tracked file can be listed, compared, and rewound to retained loops from its own last three file-touching commits, recent time window and minimum loop count, in seconds, without affecting other files.
G3. Cheap: pass-through Git commands add under 5 ms median overhead on Linux/macOS and under 20 ms median overhead on Windows (which must spawn/wait because Windows has no exec). Single-file loop medians stay under 50 ms on Linux/macOS and under 100 ms on Windows; warm 50,000-file scans stay under 500 ms on Linux/macOS and under 1 second on Windows. Store observations and fail on regressions on every OS. Windows capture budgets account for NTFS, Defender and fsync costs; durability and locking are never weakened. Local loop storage has a 500 MB default size target, subject to mandatory pinned/newest-loop protection, and per-file/time-aware retention; warn if protected loops prevent meeting the target. Zero RAM and zero CPU when nothing is being saved, except for an explicitly started `paradox watch` session, whose idle targets are under 20 MB RSS and near-zero CPU on 50,000 tracked files (record and document a Windows-specific RSS threshold if necessary).
G4. Safe: Paradox never modifies working files except through an explicit rewind (which is itself undoable), never leaks loop metadata into Git commits, never stores secrets, and never corrupts the loop store on crash or concurrent use.
G5. Visible: `paradox ui` gives a full Git GUI (graph, staging, branches, merges, stash, remotes, blame, conflict resolution) plus per-file loop timelines with two-point compare and one-click rewind.
G6. Automatic hook capture: after one `paradox init`, editor saves and supported Claude Code edits create loops without a manual capture command, using the permitted first/second-line message. Tools without a post-edit hook are covered by an explicitly started `paradox watch` session from M4.5; init never starts it.
G7. Future-ready: storage sits behind a trait so a cloud backend can be added later without rewriting anything.

NON-GOALS (do not build): any auto-started, always-on, or installed-as-a-service background process; a cloud backend (v1 is local only); fixing or shrinking the existing repo bloat (a separate problem); interactive rebase in the UI (CLI only); replacing Git's object model; a custom editor plugin or extension (we only configure existing editors' built-in or third-party run-on-save features).

## 4. KEY USER WORKFLOWS (these become the acceptance tests)

W1. AI breaks a file: I run `paradox loops backend/api.ts`, see loops with messages and times, run `paradox compare backend/api.ts 10m` to compare with 10 minutes ago, run `paradox rewind backend/api.ts 10m`. Only that file changes; the safety loop taken before the rewind lets me undo the rewind (`paradox rewind backend/api.ts undo`).
W2. Save messages: I put `//> fix button padding` on line 1 of a file and press Ctrl+S. A loop is created automatically with the message "fix button padding". If I do not change line 1 before the next save, the old message is NOT reused; that loop gets an auto-label like "+12 -3". When I commit, the `//>` line is removed from what Git records, while my working file is untouched.
W3. Commit: `paradox commit -m "msg"` stages everything and commits. `paradox commit a.ts b.ts -m "msg"` commits only those files. Afterwards, apply the per-file, time-aware retention rules in section 10; unrelated repository commits never determine retention.
W4. Browsing: `paradox timeline` shows a picker of files that are uncommitted or changed in the last 3 commits; choosing one shows that file's loops.
W5. Ignoring noise: pasting a huge new folder into the repo produces no scanning, no warnings, no loops, until I run `paradox track <path>` or `git add` it.
W6. UI: `paradox ui` opens the browser; I stage individual hunks, commit, switch branches, resolve a merge conflict, and open a file's loop timeline, click two dots, see a side-by-side compare, and rewind.
W7. Plain Git: `paradox status`, `paradox log --graph`, `paradox diff`, `paradox show`, `paradox rebase -i`, `paradox stash`, `paradox gc`, `paradox -C ../other-repo status`, `paradox -c user.name=x commit ...` all behave exactly like git (except `commit`, which auto-stages).
W8. Automatic capture: after `paradox init`, saving a tracked file in VS Code/Cursor with Ctrl+S, or editing through a supported Claude Code hook, creates a loop without a manual capture command. With no editor/AI activity and no explicitly started UI/watch session, no Paradox process exists. An explicit watch session follows its configured idle-exit rule.

## 5. DESIGN PRINCIPLES

1. Git is the source of truth; Paradox is a sidecar. Shell out to the real `git` binary (never libgit2/gitoxide) so behavior is identical by construction.
2. No always-on process. Loops are created by editor/AI hook events, by the fallback scan, or by a session-only `paradox watch` that the user starts by hand. The only long-running processes are `paradox ui` and `paradox watch`, both manually started. Nothing is installed as a service or started automatically.
3. Zero cost when unused: pass-through commands do no Paradox work (no scan, no database open).
4. Never destructive without a way back: rewinds and UI discards take a safety loop first.
5. Failures in the loop layer are logged, never fatal, and never block a Git command or an editor save.
6. Untracked is ignored: Paradox only tracks files in `git ls-files` plus explicitly added paths.
7. Boring and reliable over clever: atomic writes, WAL-mode SQLite, small modules, heavy tests.
8. No command name Paradox owns may shadow a Git subcommand, except the two documented exceptions (`commit`, `init`).

## 6. ENVIRONMENT SETUP (do this first)

The accepted development workflow edits locally and builds/tests Rust on GitHub Actions using Git and `gh`. Keep the existing local Node version unchanged. When UI work starts, develop and build the frontend locally with the existing Node/npm and test it in a local browser. A minimal local Rust compilation setup is authorized only if the UI actually needs it; do not install the large C++/Windows SDK toolchain. M1 setup is already finished and must not be restarted.
- Hosted runners provide Rust stable, cargo, rustfmt and clippy, plus native compiler prerequisites (MSVC on Windows, Xcode tools on macOS, build tools/pkg-config on Linux).
- Existing local Node/npm builds the frontend when UI work begins, with local browser testing. Hosted CI still runs the required cross-platform and frontend verification gates; it is not the primary frontend development/build environment.
- Verify and record hosted tool versions; local Git and gh must be available.
- Create throwaway test repositories OUTSIDE the project directory and do ALL Paradox testing there, including smoke tests of downloaded binaries. Never run Paradox against real repositories during development.

## 7. ARCHITECTURE

Cargo workspace:
- paradox-core: git runner (spawn git, capture output, parse porcelain formats), loop store, SQLite index, stat-cache scanner, looper (creates loops), comparer, rewinder, purger/evictor, message parser, commit-time stripper, editor-config writer, config. No CLI or HTTP concerns.
- paradox-cli: argv dispatcher, clap-based Paradox subcommands, pass-through exec, hook entry points, interactive picker.
- paradox-ui: axum server, JSON API, embedded frontend (React + Vite + TypeScript, Monaco editor and DiffEditor, built and embedded via rust-embed).
The CLI and UI call the same core functions.

Crates: clap, rusqlite (bundled), zstd, blake3, similar, inquire, anyhow, thiserror, serde, serde_json, toml, axum, tokio, rust-embed, notify (M4.5 native events), fs2 (file locking), tempfile (tests), proptest (tests), assert_cmd and predicates (tests).

Storage trait (abstract so cloud can be added later):
  trait BlobStore { put(hash, bytes), get(hash) -> bytes, delete(hash), exists(hash) }
  LocalStore implements it over <git-common-dir>/paradox/blobs/<first2>/<rest-of-blake3-hex>, zstd-compressed, written atomically (temp file + fsync + rename). The store is shared by worktrees, lives outside their working trees and requires no info/exclude entry.

SQLite schema (WAL mode, busy timeout, schema_version table with migrations):
  loops(id INTEGER PK, path TEXT, blob_hash TEXT, ts INTEGER, message TEXT, auto_label INTEGER, head_sha TEXT, pinned INTEGER, size INTEGER, kind TEXT)   -- kind: 'save' | 'safety' | 'scan' | 'deleted'; deleted records have no blob
  stat_cache(path TEXT PK, mtime_ns INTEGER, size INTEGER, last_hash TEXT)
  meta(key TEXT PK, value TEXT)
  Index on (path, ts) and on head_sha.

Config: <git-common-dir>/paradox/config.toml with defaults: size_limit_bytes=2097152, store_cap_bytes=524288000, retention_hours=24, retention_min_loops=20, secrets_denylist=[".env*","*.pem","*.key","id_rsa*"], watch_idle_minutes=30 (0 disables watch idle exit), safety_loop_commands=[] (opt-in list such as ["reset","checkout","restore","clean","stash","rebase","merge","pull"] that make a loop of uncommitted tracked changes before the destructive Git command runs). Write a tiny <store>/safety_enabled flag when that list is non-empty. Pass-through reads no config unless this flag exists; retain the original 5 ms Linux/macOS gate and accepted 20 ms Windows gate.

## 8. THE DISPATCHER (get this exactly right)

- Parse argv the way Git does: skip Git global options that come before the subcommand (`-C <path>`, `-c <k=v>`, `--git-dir=`, `--work-tree=`, `--namespace=`, `--no-pager`, `-p/--paginate`, `--bare`, `--no-replace-objects`, `--exec-path`, `--html-path`, `--version`, `--help`, etc.) to find the real subcommand.
- Paradox-owned subcommands: `commit`, `init`, `ui`, `git`, `loop`, `loops`, `compare`, `rewind`, `timeline`, `pin`, `unpin`, `track`, `untrack`, `watch` (M4.5). Handle these as their milestones arrive. For anything else, execute `git` with the ORIGINAL arguments untouched.
- Owned names always take priority over filenames. To use the file-first commit convenience for a file named like an owned command, use `paradox -- <file> commit ...` (also works with preceding Git global options). The delimiter is not forwarded as a Git global option.
- Pass-through behavior: identical stdin/stdout/stderr, exit code, colors, pager, TTY detection, signals (Ctrl+C), and environment. On Unix use exec (replace the process). On Windows spawn with inherited handles and forward the exit code.
- `paradox git <args>` always forces pure pass-through (even for `commit` and `init`).
- `paradox --help` documents the Paradox additions and states that everything else is Git.
- If the git binary is not found, print a clear error.
- Collision guard: a test must run `git help -a` (and `git --list-cmds=main,others,alias`) and fail if any Paradox-owned name other than `commit` and `init` is also a Git subcommand.

## 9. COMMANDS

Git-wrapper conveniences
- `paradox commit [flags] -m "msg"`: run `git add -A`, then `git commit` with the same flags. (Documented exception to Git parity.) Then run the purge.
- `paradox commit <file...> -m "msg"` (also accept `paradox <file> commit -m "msg"`): `git add <file...>` then `git commit <file...> -m "msg"`. Only those files.
- `paradox pull`, `paradox push`, `paradox diff`, `paradox show`, `paradox log`, `paradox gc`, and everything else: pure Git.
Loop commands (all top-level, none shadow Git)
- `paradox loop [file...]`: save a loop now for the given tracked files, or all tracked files that changed. This is what the editor and AI hooks run on every save. Single-file median under 50 ms on Linux/macOS, under 100 ms on Windows, then exit. Never prompts, never fails the caller (exit 0 on non-critical errors, logging to <git-common-dir>/paradox/log). Flags: `--gc` runs purge, eviction, and orphan-blob cleanup now; `--stats` prints store size, loop counts, oldest loop.
- `paradox loops <file>`: that file's loops (id, relative and absolute time, message, kind, pinned flag), newest first, with the committed (HEAD) version shown as the oldest entry.
- `paradox compare <file>`: working file vs its latest loop. `paradox compare <file> <a> <b>`: two versions. a/b accept a loop id, a relative time ("10m", "2h"), a message substring (error if ambiguous), `HEAD`, or `working`. Unified diff via the `similar` crate, colorized on TTY.
- `paradox timeline`: interactive picker (`inquire`) over files that are uncommitted (`git status --porcelain=v2`) OR changed in the last 3 commits (`git diff --name-only HEAD~3`; if fewer than 3 commits exist, diff against the root; if there are no commits, only uncommitted files). Selecting a file opens its `loops` view.
- `paradox rewind <file> <id|10m|HEAD|undo>`: restore only that file. First make a safety loop of the current state (kind='safety', message "before rewind"), then write the file atomically. `undo` rewinds to the most recent safety loop for that file.
- `paradox rewind --at HH:MM`: rewind all tracked files to their state at that time, with safety loops. Show a dry-run list and require confirmation (`--yes` to skip).
- `paradox pin <id>` / `paradox unpin <id>`: protect a loop from purge and eviction.
- `paradox track <path>` / `paradox untrack <path>`: add/remove paths from the tracked set beyond `git ls-files`.
Setup and UI
- `paradox init`: (documented exception) run `git init` if not already a repo; create `<git-common-dir>/paradox`, install hooks, and configure automatic loop capture (see section 10, Capture). Idempotent. Migrate an existing in-tree `.paradox` by moving it into the Git common dir, then remove the in-tree copy; never silently overwrite an existing destination store. No info/exclude entry is needed. `paradox init --uninstall` removes hooks and the editor/AI config entries that Paradox added.
- `paradox ui`: launch the UI.
- M4 also adds `paradox init --print-agent-rule [--guide-path <path>]`. Print the rule block; do not install it. The rule tells agents to write a fresh one-line message on line 1 before each edit (line 2 for shebang/`<?php` files), using the file's supported comment style and `>` (closed forms include their closing delimiter); use `paradox` instead of `git`, and commit with `paradox commit`; use `paradox loops/compare/rewind` instead of raw destructive `git reset --hard`, `checkout --` or `clean`; run `paradox track <file>` after creating a file; and, when the tool has no post-edit hook, ensure `paradox watch` is running by starting `paradox watch --background` explicitly or telling the user if it cannot start. The printed guide link points to the file supplied by `--guide-path`. Test that printing never installs or starts anything.

## 10. LOOP SEMANTICS (details that must be right)

Capture (how loops get created, with no automatically started process)
- Hook-triggered events run `paradox loop <file>` and exit:
  1. Editor save: `paradox init` merges (never overwrites) a Run-on-Save entry into `.vscode/settings.json` (extension emeraldwalk.runonsave, command `paradox loop "${file}"`, `isAsync: true` so editor saves never wait), and adds that extension to `.vscode/extensions.json` recommendations. Works in VS Code and Cursor. Test that the generated command is asynchronous.
  2. AI edits: `paradox init` merges a PostToolUse hook for Edit/Write/MultiEdit into `.claude/settings.json` running `paradox loop "$file"`.
  3. Other editors: `paradox init --print-editor-config neovim|jetbrains|sublime` prints the snippet (Neovim BufWritePost autocmd, JetBrains File Watcher, etc.).
- Fallback scan: every Paradox command that touches loops, and the pre-commit hook, first runs a Git-style stat scan over the tracked set and makes a loop (kind='scan') for any tracked file changed since its last loop. Worst case, intermediate saves are lost, never the final state.
- Baseline: the committed (HEAD) version counts as the oldest version for `loops`, `compare` and `rewind`. Do not save loops of the whole repository at init.
- M4.5 adds the manually started session-only `paradox watch`, required for tools/agents with no post-edit hook. Full lifecycle, native-event, debounce, bulk, suppression, refresh and test requirements are in `docs/milestones/M4.5-watch.md`; those requirements are part of this brief. Do not implement it before M4 is complete.
Tracked set: `git ls-files` plus `track` paths. Untracked files and new directories are ignored entirely and never scanned.
Change detection (Git-style): compare mtime_ns and size against stat_cache; hash with blake3 only if they differ. Pass-through commands never scan.
Dedup: skip making a loop if the hash equals that file's latest loop (editors with autosave must not inflate storage).
Skips: binary files (NUL byte in first 8 KB), files over size_limit, and denylisted secrets. Never store secrets.
Messages: support `//>`, `#>`, `-->`, `-- >`, `;>`, `%>`, `/*> message */` and `<!--> message -->` on line 1. Closed-comment forms include their closing delimiter on that same line. Use the message only if the permitted message line differs from the previous loop's; otherwise auto-label (e.g. "+12 -3"). A shebang or `<?php` uses line 2. JSON always auto-labels. Never look beyond the permitted line 1/2; support CRLF, Unicode, stale markers and malformed/unfinished markers safely.
Commit-time stripping: the marker line must never reach Git, and the working file must never change. All message forms above must be removed entirely, including closing `*/` and `-->`, preserving the remaining bytes exactly. In the pre-commit hook and in `paradox commit`, compute stripped staged content, write it with `git hash-object -w --stdin`, and stage it with `git update-index --cacheinfo <mode>,<sha>,<path>`. Preserve file mode, line endings and trailing newline. Files without valid markers are untouched; the hook must be a fast no-op when no staged file has a marker.
Purge: retention is per file and time-aware. KEEP a loop if ANY applies: (a) newer than that file's most recent commit (uncommitted work); (b) belongs to one of the last three commits touching that file (`git log -n3 -- <path>`); (c) newer than retention_hours (default 24); (d) among that file's newest retention_min_loops loops (default 20); (e) pinned. Purge only loops meeting none of these, then delete orphaned blobs. Never use repo-wide HEAD ancestry or the number of unrelated commits; pulling other people's commits must not purge local work.
Eviction: if store size exceeds store_cap_bytes, evict oldest unpinned loops first, but NEVER a file's newest loop. If protected newest loops/pins/metadata prevent meeting the cap, retain them and warn instead. The cap is a target subject to this mandatory protection.
Renames: Git renames keep history if detected by `git status` rename info; otherwise history stays under the old path (document this). A tracked file disappearing records kind='deleted', a timestamp and no blob; display it in history. Reappearance at the same path continues that history. Rewind can restore a version before deletion.
Branch switches and history rewrites: loops are keyed by path and carry head_sha for provenance; they do not move with branches. Document this behavior.
Hooks: install into the active hooks directory (respect `core.hooksPath`). If a hook already exists, chain it (run the existing hook first, then Paradox), never overwrite.
Safety loops (opt-in for Git commands): if `safety_loop_commands` lists a Git subcommand and the user runs it via `paradox`, make loops of modified tracked files (via `git diff --name-only`) before executing it.
Concurrency and crash safety: advisory lock at `<store>/lock`; SQLite WAL with busy timeout; all blob and file writes are temp+fsync+rename; recover partially written state with `paradox loop --gc`. For `paradox loop`, try-lock with a 15 ms timeout. On contention, write a tiny path marker under `<store>/pending/` and exit 0. The lock holder drains pending markers before release, reading current contents so bursts coalesce and the final state is captured. Contention must not block a save beyond its 50 ms budget; test 20 parallel savers plus the watcher. All worktrees share `<git-common-dir>/paradox`, with paths scoped by worktree.

## 11. THE UI (`paradox ui`)

Server: axum bound to 127.0.0.1 on a random free port; random per-session token required on every request; strict Host-header and Origin checks (DNS-rebinding protection); no CORS; opens the default browser; exits on Ctrl+C or when the last tab disconnects. Frontend is embedded in the binary.
All Git actions call the real `git` through the same core runner as the CLI. Parse machine-readable output only (`status --porcelain=v2 -z`, `log` with custom --format and NUL separators, `for-each-ref`, `diff --numstat -z`, `blame --porcelain`). Never parse human-formatted output.
Git features:
- Commit graph with branch/merge lanes, tags, remote refs, HEAD marker. Virtualized and paginated (load pages of ~200 commits via `--skip/--max-count` or cursor); must stay smooth on a repo with 100k+ commits and 6 GB of history. Never load full history.
- Working tree: changed/untracked list, stage/unstage whole files and individual hunks or lines (via `git apply --cached` with generated patches), discard with confirm, commit box with amend.
- Branches: create, checkout, rename, delete (with unmerged warning), merge, rebase (non-interactive), cherry-pick, revert. Tags: create/delete. Stash: list, apply, pop, drop, create.
- Remotes: fetch, pull, push with live progress output. Force push is `--force-with-lease` only, never plain `--force`.
- Commit detail: file list, side-by-side diff (Monaco DiffEditor), blame, per-file history.
- Merge conflict resolver: three-way view (ours/theirs/result), accept-either, manual edit, mark resolved.
- Interactive rebase: out of scope for v1; show a clear message pointing to the CLI.
Loop features:
- File panel (uncommitted + last-3-commit files). Selecting a file shows its loop timeline: dots labelled with messages, commit boundaries as vertical dividers, pinned icon, the HEAD version as the first dot. Click one dot to compare against the working file; click two to compare them; rewind button with confirm; pin/unpin.
- Final milestone: an all-files swimlane view (one lane per file on a shared time axis) with "rewind all files to this timestamp" (dry-run preview, confirm).
Safety and transparency: every destructive action (discard, reset, branch delete, force push, rewind) shows a confirm dialog listing exactly what will be lost, makes a safety loop where possible, and shows a reflog hint otherwise. A collapsible command log panel shows the exact `git` command executed for every action. Respect light/dark system theme.
JSON API (all under /api, token-protected): GET /status, GET /graph?cursor=, GET /commit/:sha, GET /diff, POST /stage, POST /unstage, POST /commit, branch/tag/stash/remote endpoints, GET /loops/files, GET /loops/files/:path, GET /loops/compare?path=&a=&b=, POST /loops/rewind, POST /loops/pin. Long-running Git operations stream progress (SSE).

## 12. TESTING (write these as you build; all must pass before each milestone is reported)

Unit tests
- Message parser: every comment style, including `-- >`, `/*> message */` and `<!--> message -->`; stale-message rule, shebang/`<?php` line-2 rule, JSON/no-comment files, CRLF, empty files, long lines and Unicode. Use proptest across every form to prove no panic and no message reads beyond line 1/2. The M4 stripper tests must cover complete removal of closed forms, preserving every remaining byte and file mode.
- Dedup, size/binary/secret skipping, oldest-first eviction protecting every file's newest loop, pins, per-file commit windows, 24-hour retention and minimum loop counts. A file edited two hours ago survives unrelated commits; pulling 50 commits purges no local work. Test deleted/no-blob records, reappearance and rewind before deletion.
- Tracked-set logic: untracked and new directories ignored; `track` adds; `untrack` removes.
- Stat-cache: unchanged mtime/size causes no hashing; touch without content change causes hash but no new loop.
- Stripper: strips only the marker line, preserves file mode, line endings, and trailing newline; no-op when no marker.
- Dispatcher routing: table-driven tests over argv including `-C`, `-c`, `--git-dir`, `--no-pager`, `--version`, `-h`, `commit` vs `git commit`, `paradox <file> commit`, owned-name priority and `paradox -- <file> commit ...`, plus Git subcommands that look similar to Paradox names (`diff`, `show`, `log`, `gc`, `restore`). Fuzz argv to ensure Git flags are never misparsed and original args reach git byte-for-byte.
- Collision guard: Paradox-owned names (except `commit`, `init`) never appear in Git's command list.
- Relative time and message-substring resolution for compare/rewind, including ambiguity errors and the `HEAD`, `working`, `undo` keywords.
- Editor-config writer: `paradox init` merges into existing `.vscode/settings.json` and `.claude/settings.json` (including files with comments/trailing commas where the format allows), is idempotent, preserves unrelated settings, and `--uninstall` removes only what it added.
Integration tests (assert_cmd, temp repos)
- Parity: for a list of 40+ Git commands/flag combos (status, log --graph --oneline, diff, show, branch, stash, rebase non-interactive, cherry-pick, tag, config, gc, worktree add, submodule status, etc.), run `git X` and `paradox X` in identical repos and compare stdout, stderr, and exit codes.
- Workflow tests W1-W8 above end to end, including: loop, edit, compare, rewind, undo-rewind; `paradox commit` then `git show HEAD` must NOT contain any `//>`-style marker line while the working file still does; purge removes only old unpinned loops; rewind touches only the named file; a simulated editor save (invoking the configured command) creates a loop with the line-1 message.
- Hooks: install, chain with an existing hook, honor core.hooksPath, uninstall; raw `git commit` (not via paradox) still strips markers and purges.
- Crash safety: kill the process mid-loop and mid-purge (inject failure points); the store must remain consistent and `paradox loop --gc` repairs it. Twenty concurrent `paradox loop` processes (several saves in one second) must not corrupt the index or lose the last save.
- Worktrees and submodules do not break pass-through or loops.
- Large file/binary/secret files never get loops.
- Process-lifecycle check: no command auto-starts any background process. Only explicitly started `paradox ui` and `paradox watch` sessions may remain running. A watcher must leave no process after Ctrl+C, `--stop` or idle exit; test foreground and explicitly requested background sessions.
Performance benchmarks (fail the build on regression)
- Pass-through overhead vs plain git under 5 ms median on Linux/macOS and under 20 ms on Windows; also enforce the stored per-OS regression baseline. Interleave Git and Paradox in one test process within each assessment, with at least 200 measured samples each after warm-up. CI schedules three consecutive assessments on fresh hosted runners using the same precompiled release binary and Rust harness. Compare the median signed paired timing difference; fail a command only if its unchanged ceiling is exceeded in at least two of three assessments. Always retain all three results and raw pairs; missing/invalid reports fail. Direct local release tests use three consecutive rounds in one process. Keep stored baseline observations unchanged and capture performance a separate strict gate without rerun voting.
- `paradox loop <file>` on 50,000 tracked files: median under 50 ms on Linux/macOS, under 100 ms on Windows. Warm stat scan median under 500 ms on Linux/macOS, under 1000 ms on Windows. Enforce stored per-OS capture baselines as well as these limits; atomic writes, fsync, SQLite synchronous mode and locking remain mandatory.
- Synthetic large repo (use `git fast-import`, 100k+ commits, multi-GB if feasible, otherwise scaled with documented extrapolation): UI graph first page under 300 ms, smooth paging.
UI tests
- API tests for each endpoint against temp repos; token/Host/Origin rejection tests.
- Frontend component tests (Vitest) and end-to-end flows (Playwright): stage a hunk, commit, create/switch branch, resolve a conflict, open a loop timeline, compare two loops, rewind.
Quality gates
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo audit` or `cargo deny`, frontend lint and type-check, all green.
- CI configuration (GitHub Actions) running the full suite on Windows, macOS, and Linux.

## 13. MILESTONES AND ACCEPTANCE CRITERIA (stop after each; show a summary, demo output, and test results)

M1. Setup + workspace + dispatcher. Accept: toolchain installed and verified; `paradox status`, `paradox log --graph`, `paradox diff`, `paradox rebase -i`, `paradox -C <dir> status` match git exactly (output, exit codes, colors, pager, editor); dispatcher tests, collision guard, and parity suite pass; overhead benchmark under 5 ms on Linux/macOS and under 20 ms on Windows.
M2. Store + index + stat cache + `paradox loop`. Accept: message parsing, dedup, skip rules, atomic writes, WAL, locking; per-OS capture and warm-scan gates plus baseline regression checks from section 12; concurrency and crash-safety tests pass.
M3. `loops`, `compare`, `rewind` (including undo and `--at`), `pin`, `track`, `loop --gc/--stats`. Accept: W1 and W5 pass; rewind is undoable; rewind touches only the named file; HEAD baseline works.
M4. `paradox init` with automatic capture (editor and AI hooks), agent-rule printing, `paradox commit` (+ `<file> commit`), git hooks, stripping, purge, eviction. Accept: W2, W3 and W8 pass; every marker form is absent from `git show HEAD` while worktree bytes are untouched; hook chaining, core.hooksPath, editor-config and print-only agent-rule tests pass; no command auto-starts a process.
M4.5. `paradox watch`: implement and pass every requirement/test in `docs/milestones/M4.5-watch.md`, including manual foreground/background lifecycle, PID/lock recovery, index/track-list refresh, native events, per-file debounce, self-write suppression, bulk guard, multi-agent-safe capture and measured idle footprint. Windows and macOS CI must run the watcher tests.
M5. `paradox timeline` picker. Accept: W4 passes with 0, 1, 2, and 3+ commits.
M6. UI part 1: server, security, commit graph, working tree, staging (files and hunks), commit, diff. Accept: API and Playwright tests; graph pages smoothly on the synthetic large repo.
M7. UI part 2: branches, merge/rebase/cherry-pick/revert, stash, remotes with progress, blame, conflict resolver, destructive-action confirmations, command log panel.
M8. UI part 3: loop timeline with one/two-point compare and rewind, then the swimlane view and rewind --at. Accept: W6 passes end to end.
M9. Hardening: cross-platform CI green, README (install, quickstart, command reference, design notes, known limitations, how to alias git=paradox, how automatic capture works and how to set up other editors), release build instructions for a single binary per OS.

## 14. WORKING AGREEMENT

- Build in the order above. Do not skip ahead. Commit your own work frequently in the Paradox project repo with clear messages.
- Never touch my real repositories during development. Use only the throwaway repo.
- When something is ambiguous but low-stakes, choose the safest option, document it in the README, and keep going. Ask me only when a decision would change what the product does.
- At each milestone report: what was built, what tests ran and their results, benchmark numbers, known issues, and what comes next.
- If a requirement here conflicts with Git parity or data safety, prefer parity and safety, and tell me.

## Accepted amendments (October 15, 2026)

Current owner instruction: pause UI implementation while applying and testing
these core follow-ups. Stop at the accepted M7 boundary. M8 (loop UI) and M9
(release hardening) remain pending for later; do not begin them now. Previously
closed milestones receive focused follow-up fixes and regression tests rather
than being restarted. The storage, retention, lock contention, pass-through,
dispatcher and deletion rules above supersede their earlier versions.

For the rest of development, when all functional tests pass and a performance
or memory gate misses by under 10%, document the exception, set a realistic
OS-specific CI threshold with stored baseline-regression protection, and
continue without asking. Ask the owner only for a miss over 10% or a
safety/durability tradeoff. Record the measured value, original limit, percentage
miss, reason, new limit and baseline in the milestone report and benchmark data.
Keep safety, atomic writes, fsync and locking requirements intact. This latest
rule supersedes earlier instructions to keep a milestone open for every small
performance or memory overage; it does not waive functional failures. Existing
limits stay in place unless a measured exception is documented.

The owner subsequently authorized continuing directly from accepted M4.5 into
M5 and the remaining milestones, while reporting each boundary. The temporary
battery-related request to stop after M4.5 was explicitly withdrawn.

M2's accepted Windows-only capture budgets are under 100 ms per single-file loop
and under 1000 ms per warm 50,000-file scan. Linux/macOS retain 50/500 ms. Store
per-OS observed baselines and enforce regressions with the documented scheduling
allowance, capped by the OS limits. One bounded cheap-win pass is allowed before
re-measurement and closure; no weaker atomic writes, fsync, SQLite synchronous mode
or locking. Keep fat LTO, one codegen unit, abort-on-panic and static Windows CRT.
The M4 Run-on-Save entry must use `isAsync: true`. Latest watcher/message/principle
amendments supersede older text. Close M2 after validation and continue.

Build and test Rust on GitHub Actions, using Git and gh. UI development and Node/npm builds run locally with browser testing, using the existing Node version. A minimal local Rust setup is permitted only if UI work actually needs it; the large C++/Windows SDK install remains excluded. Use aditya raj / Adiiitu12312 / en3my20@gmail.com for project commits. M1 accepts the Windows G3 timing exception described above. The stored observation allows 50% plus 1 ms scheduling variance, never above the OS limit. The session-only watcher is now required at M4.5 (after M4, before M5), replacing the earlier optional post-M9 proposal. Milestone order is M1, M2, M3, M4, M4.5, M5, M6, M7, M8, M9. Stop and report at each boundary. Finished work is not restarted.

M4 boundary acceptance, October 15, 2026: the owner allows M4 to close with a
documented macOS hosted-timing exception. Keep the strict 5 ms macOS CI gate,
stored baselines and visible failed results unchanged. This is a boundary
exception for M4 only, not a new performance budget or permission to waive
later milestone gates. Functional, terminal, capture and audit validation remains
required; provenance and failed timing observations are in docs/milestones/M4.md.

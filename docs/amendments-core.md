# Core amendments: implementation and validation in progress

The owner's latest instruction is to complete these amendments and all M7
acceptance work, then stop. M8/M9 and S (cloud-system exploration) remain pending;
no cloud research starts before M7 acceptance. These focused changes amend earlier milestones
without restarting their finished work.

The store moves to `<git-common-dir>/paradox` (`.git/paradox` in a normal repo).
Worktrees share it, while loop paths remain scoped to each worktree. Init moves
the old in-tree `.paradox` directory intact and refuses to overwrite a second
store. Stop any older watcher before migration. The new location needs no
`info/exclude` entry; migration removes only the exact old Paradox exclusion.
Hooks rediscover this location through Git rather than embedding a working-tree
store path. The watcher ignores Git metadata and legacy metadata paths. A stable
lock in the Git common directory serializes migration. On Windows, after checking
both old store/watch locks, init closes its old handles immediately before the
atomic directory rename while retaining the common migration lock. Older binaries
must remain stopped during migration; they cannot honor the new migration gate.

Retention keeps a loop when any protection applies: it is newer than that
file's latest commit, belongs to its last three file-touching commits, is under
`retention_hours` (24 by default), is among its newest `retention_min_loops`
(20 by default), or is pinned. Other files' commits and unrelated pulls cannot
shorten this window. Unknown or rewritten provenance is retained conservatively.
Eviction removes oldest unpinned loops while protecting each file's newest
loop. If that prevents reaching the size target, Paradox warns and keeps them.

Contended save commands try the store lock for 15 ms, publish a tiny pending
path marker and exit successfully. The holder reads current contents and
coalesces markers before releasing. A post-release check and a publisher's
nonblocking takeover close the empty-queue/release race. No daemon or worker
process is started. Pending markers contain paths, not file contents.

The initial macOS 20-saver burst measured a 17.047125 ms median lock wait and
78.886042 ms durable-marker exit (run 38770132710, bbf0076). This exceeded the
original 50 ms contention budget by 57.77%. The owner authorized choosing a
practical exception: macOS has a 100 ms limit, with the stored baseline enforcing
the tighter 95.6632504 ms regression ceiling. Windows subsequently measured
135.0832 ms (170.17% over 50 ms), with 20.2242 ms median lock wait, in run
38772397963, 4f63bba. The owner authorized a practical Windows-only 200 ms limit;
the first stored-baseline ceiling was 163.09984 ms. A later debug run measured
185.3625 ms; Windows now uses the same 1.5 scheduling allowance as the existing
capture/pass-through gates, capped at the already-approved 200 ms limit. The
135.0832 ms baseline is retained. Linux keeps 50 ms. The lock
timeout remains 15 ms and atomic writes, fsync and SQLite durability are unchanged.
The exception concerns simultaneous contended saves, not the normal single-save
or scan gates. Initial observations were debug-profile concurrency tests; CI
enforces timing on the optimized release profile; debug tests enforce all
coalescing/content/integrity assertions and retain diagnostic timings. See
`benchmarks/pending.csv` and `pending-observations.csv`; failed measurements
remain recorded.

The first fresh-host release assessment (38782358592, 6f074c0) passed normal
Windows capture at 72.7786 ms and warm scanning at 475.2726 ms, but its contended
burst measured 240.2241 ms and failed the 200 ms gate. The same pending harness
measured 53.0967 ms locally. Both observations are retained. The follow-up avoids
redundant directory creation/validation while keeping atomic publication, fsync,
locking and symlink checks. No contention or normal-capture threshold changed.

Pass-through reads no Paradox config unless `<store>/safety_enabled` exists.
After editing `safety_loop_commands`, run `paradox init` or another owned
store command to synchronize that flag. Empty configuration disables the flag;
the default route retains native Git behavior and existing 5/20 ms OS gates.
`paradox git ...` always bypasses opt-in capture too.

Owned command names always win over files. Use `paradox -- loop commit -m "..."`
to commit a file named `loop`; `paradox -C repo -- commit commit -m "..."`
handles a file named `commit`. The escape delimiter is not passed as a global
Git option. Ordinary `paradox path.ts commit ...` remains supported.

A disappearing tracked file creates a `deleted` loop with a timestamp and no
blob. Repeated missing-file events deduplicate. Reappearance at the same path
continues the history, and rewind can restore an earlier saved version.

Validation is in progress. Do not treat the implementation notes here as a
claim that the new cross-platform regression or performance gates have passed.

The bounded metadata optimization passed all functional checks, but the next
fresh Windows host measured 299.6146 ms for the 20-saver burst, 49.8073% over
200 ms. Normal capture passed at 81.2546 ms. Under the owner's delegated practical
choice, the final Windows-only contention limit is 400 ms, with the reviewed
299.6146 ms baseline enforcing a tighter 360.53752 ms ceiling. All previous
observations remain retained; no data-safety guarantee or normal capture budget
changes. This supersedes the earlier 200 ms contention limit described above.

Regression coverage lives in `crates/paradox-core/tests/store_amendments.rs`
(migration, per-file retention, deletion and eviction), `tests/pending.rs`
(handoff, shared worktrees and SQLite succession), and
`crates/paradox-cli/tests/amendments_dispatch_safety.rs` (dispatcher/config fast
path). The CLI watcher suite exercises 20 parallel savers with watch running;
`continue_hooks.rs` covers native merge/rebase marker handling.

# Core amendments: implementation and validation in progress

UI implementation is paused at the owner's request. M6/M7 acceptance remains
pending; M8/M9 are deferred. These focused changes amend earlier milestones
without restarting their finished work.

The store moves to `<git-common-dir>/paradox` (`.git/paradox` in a normal repo).
Worktrees share it, while loop paths remain scoped to each worktree. Init moves
the old in-tree `.paradox` directory intact and refuses to overwrite a second
store. Stop any older watcher before migration. The new location needs no
`info/exclude` entry; migration removes only the exact old Paradox exclusion.
Hooks rediscover this location through Git rather than embedding a working-tree
store path. The watcher ignores Git metadata and legacy metadata paths.

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

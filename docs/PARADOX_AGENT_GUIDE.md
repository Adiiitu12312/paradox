# Working with Paradox

Use `paradox` for Git commands and `paradox commit` to commit. Ordinary Git
operations use the installed Git binary. `paradox git <args>` is the explicit
escape for pure Git behavior.

Before editing a tracked file, write a fresh one-line description on its first
line using its comment syntax followed by `>`. Examples are `//> fix validation`,
`#> handle empty input`, `-- > add index`, `;> update config`, `%> fix equation`,
`/*> adjust spacing */` and `<!--> fix layout -->`. Include the closing delimiter
for CSS and HTML forms. After a shebang or a standalone `<?php`, use line two.
Never insert comments into JSON; Paradox labels those edits automatically. A
message left unchanged on the next save is stale and gets an automatic diff label.

After creating a new file, run `paradox track <file>` so saves can create loops
before Git stages it. New folders otherwise remain ignored until explicitly
tracked or added to Git. Existing Git ignore and secret-denylist rules still apply.

Run `paradox loops <file>` to inspect versions, `paradox compare <file> <version>`
to inspect changes, and `paradox rewind <file> <version>` to restore one file.
Versions can be a loop ID, relative time such as `10m`, an unambiguous message,
or `HEAD`. A rewind creates a safety loop; `paradox rewind <file> undo` restores
it. Do not undo work with raw `git reset --hard`, `checkout --` or `clean`.

After `paradox init`, VS Code/Cursor's recommended Run-on-Save extension and
supported Claude PostToolUse hooks invoke capture. When a tool has no post-edit
hook, ensure a manual watch session is running: explicitly start
`paradox watch --background`, or tell the user if it cannot be started. Watch
arrives in M4.5, immediately after M4; until then, invoke `paradox loop <file>`
after each edit and report that watch is unavailable. Nothing starts it
automatically. Once available, stop a session with `paradox watch --stop` or
Ctrl+C in foreground mode.

Commit with `paradox commit -m "message"`, or name files to commit only those
paths. Git's staged content loses valid marker lines; working files keep them.
Retained markers may therefore appear as working changes after commit. Never
edit `.paradox` directly or stage its contents.

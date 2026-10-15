# Optional milestone after M9: session-only `paradox watch`

Requested on October 15, 2026. This is deferred until after M9 and is not part of
the current CLI or capture implementation. M1–M9 keep event-triggered capture
without an always-running process.

## Session behavior

- The user starts `paradox watch` manually. It prints a one-line running notice.
- Never auto-start it, register it as a service or add it to editor/AI setup.
- Stop with Ctrl+C or `paradox watch --stop`.
- Refuse a second watch instance in the same repository.
- Exit automatically after a configurable interval without change events;
  the default idle interval is 30 minutes.

## Event capture

- Use `notify` with native OS events: inotify on Linux, FSEvents on macOS and
  ReadDirectoryChangesW on Windows. No polling.
- Debounce events for 300 ms.
- Watch only the tracked set: `git ls-files` plus explicitly tracked paths.
  Never watch the whole repository tree. Skip node_modules, build output and
  anything gitignored.
- On Linux, if file watches would exceed the inotify limit, fall back to
  directories containing tracked files, filter events to the tracked set, and
  print a clear warning.
- Every relevant event calls the same capture implementation as
  `paradox loop <file>`, retaining its deduplication, skip rules and message rules.
- Extend the Git command collision guard before adding `watch` to owned commands.

## Acceptance tests

- A save from a non-hooked tool, such as `sed -i`, creates a loop while the
  session runs.
- No watch process remains after Ctrl+C, `--stop` or idle exit.
- Two instances in one repository are refused.
- On a repository with 50,000 tracked files, measure resident memory after ten
  minutes idle. Target less than 20 MB RAM and approximately zero idle CPU.
- Verify native events, tracked-set filtering, ignore filtering, debounce behavior
  and the Linux watch-limit fallback.

# Paradox code size

CLOC 2.06 measured source `ca8b4a9ac0f070d16c0778711d13fb5f98f1a06a`
on October 16, 2026 (Asia/Calcutta). Counts exclude blank lines and comments.

| Scope | Code lines | Files |
| --- | ---: | ---: |
| Application source, including inline Rust unit tests | 11,631 | 46 |
| Separate test files and test setup, including Python test helpers | 8,357 | 39 |
| Build/CI scripts and frontend configuration | 568 | 8 |
| Total | 20,556 | 93 |

The total contains 18,264 Rust, 965 TypeScript, 1,068 Python, 158 CSS,
87 JavaScript and 14 HTML code lines. CLOC separately counted 397 comment
lines and 699 blank lines.

This counts Paradox's wrapper, browser UI, tests and support code. It does not
count Git's implementation, vendored dependencies, `node_modules`, generated
assets, build output, lockfiles or documentation. Inline Rust unit tests remain
in the application-source figure because CLOC classifies whole files.

The input is Git-tracked `.rs`, `.ts`, `.tsx`, `.css`, `.html`, `.py`, `.mjs`
and `.js` files under `crates`, `frontend` and `scripts`, excluding generated or
dependency directories. Reproduce with CLOC's `--list-file` option against that
file list. The machine-readable local report is `.artifacts/cloc-all.json`.

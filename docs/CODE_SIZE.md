# Paradox code size

CLOC 2.06 measured source `ab34e084344539999b5b08f61821a67313e57048`
on October 15, 2026. Counts exclude blank lines and comments.

| Scope | Code lines | Files |
| --- | ---: | ---: |
| Application source, including inline Rust unit tests | 11,621 | 46 |
| Separate test files and test setup | 7,749 | 36 |
| Build/CI scripts and frontend configuration | 1,002 | 10 |
| Total | 20,372 | 92 |

The total contains 18,252 Rust, 965 TypeScript, 896 Python, 158 CSS,
87 JavaScript and 14 HTML code lines. CLOC separately counted 393 comment
lines and 679 blank lines.

This counts Paradox's wrapper, browser UI, tests and support code. It does not
count Git's implementation, vendored dependencies, `node_modules`, generated
assets, build output, lockfiles or documentation. Inline Rust unit tests remain
in the application-source figure because CLOC classifies whole files.

The input is Git-tracked `.rs`, `.ts`, `.tsx`, `.css`, `.html`, `.py`, `.mjs`
and `.js` files under `crates`, `frontend` and `scripts`, excluding generated or
dependency directories. Reproduce with CLOC's `--list-file` option against that
file list. The machine-readable local report is `.artifacts/cloc-all.json`.

# iana-time-zone 0.1.65: on-demand CoreFoundation patch

Source: crates.io iana-time-zone 0.1.65, archive SHA-256 matches Cargo.lock. Upstream MIT/Apache licenses, README and platform implementations are retained.

The Darwin timezone algorithm is unchanged: reset the cached system timezone, copy its current zone, borrow its name, use the native UTF-8 pointer or byte-conversion fallback, reject invalid/oversized names, release the copied zone. Only the seven CoreFoundation calls are routed through a complete OnceLock-cached typed dlopen/dlsym table at the official absolute system path. Handles remain alive for the process. Preflight failure returns the existing OsError, with no panic or missing-function call. core-foundation-sys retains the exact upstream ABI types with its default eager-link feature disabled. Other platforms are unchanged.

The isolated package adds native table/missing-symbol tests, direct/fallback name-conversion agreement, usable IANA identifier checks and known DST gap/overlap checks. Cargo.toml excludes omitted packaging examples and adds standalone workspace isolation; generated lock/target output is ignored. The workspace patches this package so local timezone support does not eagerly load CoreFoundation during pass-through commands. Replacing local timezone algorithms or reading undocumented system configuration is deliberately avoided.

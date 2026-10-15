# Inactive notify 8.2.0 diagnostic patch

Source: crates.io notify 8.2.0, archive SHA-256 matches Cargo.lock. Upstream LICENSE-CC0 and README are retained. No upstream native event translation, latency, callbacks, scheduling, path handling or watcher lifecycle is changed.

Only src/fsevent.rs differs: preflight the fully lazy fsevent-sys tables in the shared constructor; use two accessors for real CoreFoundation exported DATA. All other sources are upstream. Cargo.toml adds standalone workspace isolation, .gitignore excludes generated local lock/target output. No product Cargo patch is active. The manual comparison workflow supplies runner-only patches for all three packages.

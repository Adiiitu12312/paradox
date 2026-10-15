# notify 8.2.0: on-demand macOS framework patch

Source: crates.io notify 8.2.0, archive SHA-256 matches Cargo.lock. Upstream LICENSE-CC0 and README are retained. No upstream native event translation, latency, callbacks, scheduling, path handling or watcher lifecycle is changed.

Only src/fsevent.rs differs: preflight the fully lazy fsevent-sys tables in the shared constructor; use two accessors for real CoreFoundation exported DATA; replace its separate CFRunLoopIsWaiting extern declaration with the identical late-bound unsigned-byte Boolean API. All other sources are upstream. Cargo.toml adds standalone workspace isolation, .gitignore excludes generated local lock/target output. The workspace patches this package together with fsevent-sys and iana-time-zone to avoid eager framework startup in pass-through commands. The controlled comparison and normal native watcher acceptance tests validate the change.

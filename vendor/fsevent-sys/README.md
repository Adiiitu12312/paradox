# fsevent-sys 4.1.0: inactive lazy-CoreServices candidate

Upstream: <https://github.com/octplane/fsevent-rust/tree/master/fsevent-sys>. Copyright and MIT license remain in LICENSE. This candidate is not patched into the Paradox workspace. The manual comparison workflow supplies a runner-only Cargo patch for measurements.

The original constants, context layout and 20 public unsafe extern C function signatures remain intact. CoreFoundation bindings are byte-for-byte upstream. The CoreServices link attribute is replaced by a OnceLock-cached typed function table, loaded from the official CoreServices umbrella absolute path with dlopen RTLD_NOW|RTLD_LOCAL. The single library reference remains alive until process exit, including partial-load errors; native event handling performs no further dlopen, symbol lookup or polling.

Before product adoption, macOS watch startup MUST call ensure_loaded() before constructing notify's watcher and report its error normally. No such product wiring is included in this candidate. All wrappers additionally return neutral null/zero/no-op values if cached loading failed, avoiding calls through missing pointers or panics at the C boundary; fallback values are not a replacement for preflight.

The added macOS unit test resolves the complete table and checks a nonexistent symbol returns an error. Adoption requires measured startup improvement, no eager CoreServices load command, and all ordinary watcher/capture/terminal/footprint gates. The standalone workspace declaration keeps this candidate out of parent workspace membership.

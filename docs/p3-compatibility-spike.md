# WASI Preview 3 compatibility spike

Verified on macOS arm64 on 2026-08-23:

1. A Rust `cdylib` with `wit-bindgen = 0.60.0` and async WIT exports builds with
   `nightly-2026-08-23 --target wasm32-wasip2`.
2. `wasm-tools 1.257.1 validate` accepts the output.
3. A stable-Rust host with `wasmtime = 48.0.0` and
   `wasmtime-wasi = 48.0.0` instantiates it and calls the async `manifest`
   export.

The host must enable the Component Model and its async support. The guest built
for `wasm32-wasip2` imports P2 WASI interfaces, therefore the host links P2 in
addition to P3. Plugins receive no directory, network or environment grants.

The spike currently establishes macOS compatibility only. The same exact tuple
must be run on Linux before the plugin ABI is released as portable.

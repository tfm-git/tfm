# Compatibility contract

The host and CLI use stable Rust. WASM analyzer plugins will use a separately
pinned nightly toolchain until Rust can build WASI Preview 3 components on
stable.

| Surface | Pinned value |
| --- | --- |
| Rust host minimum | 1.97 |
| Rust host toolchain policy | current stable |
| Wasmtime / wasmtime-wasi | 48.0.0 |
| wit-bindgen | 0.60.0 |
| wasm-tools | 1.257.1 |
| Plugin ABI | `tfm:plugin@0.3.0` |
| Plugin toolchain | `nightly-2026-08-23` (`rustc 1.100.0-nightly`) |
| Plugin target | `wasm32-wasip2` |

The compatibility spike on macOS built an async Rust analyzer plugin with this
toolchain and ran its `manifest` export in a Wasmtime 48 host. The host enabled
the component model and links both `wasmtime-wasi` P2 (guest target support) and
P3.

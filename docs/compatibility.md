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
| Plugin ABI | `tfm:plugin@0.1.0` |

The exact WASI Preview 3 WIT snapshot, plugin nightly date, and component build
target are deliberately not locked until a compatibility spike proves that a
plugin built with the tuple runs in Wasmtime 48 on macOS and Linux.

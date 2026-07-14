# Elixir + GPUI Web demo

This demo uses Phoenix for a JSON API and static asset delivery, while the
browser UI runs as Rust-compiled WebAssembly through GPUI Web. Rustler is not
used because Rust executes in the browser rather than inside the BEAM.

## Architecture

```text
Browser
  └─ GPUI Web (Rust → WebAssembly → WebGPU canvas)
       └─ fetch /api/status
            └─ Phoenix / Elixir
```

`gpui_web` is an unpublished Zed workspace crate, so the Zed and component
library dependencies are pinned to compatible revisions. Phoenix supplies
COOP and COEP response headers for GPUI Web's browser requirements. The demo
uses the single-threaded dispatcher for runtime stability.

The UI widgets are implemented with [Guise](https://github.com/wess/guise).

## Requirements

- Elixir 1.17 or later and OTP 27 or later
- rustup and Rust nightly
- [Trunk](https://trunkrs.dev/) 0.21
- A browser with WebGPU enabled; current Chrome or Edge is recommended

Install the browser UI toolchain once:

```sh
rustup toolchain install nightly --component rust-src --target wasm32-unknown-unknown
cargo install trunk --locked
```

## Run

```sh
mix setup
mix ui.build
mix phx.server
```

Open <http://localhost:4000>. Select **Connect to Phoenix API** to retrieve the
Elixir version and OTP release from the backend.

### Linux Chromium: `No available adapters`

Chromium on Linux may expose `navigator.gpu` while blocking every GPU adapter.
First enable graphics acceleration in the browser settings and restart the
browser completely. For local development, enable experimental WebGPU support
if no adapter is still available:

```sh
chromium \
  --enable-unsafe-webgpu \
  http://localhost:4000
```

Use `chrome://gpu` to confirm that WebGPU is hardware accelerated. The current
GPUI Web implementation requires WebGPU and does not automatically fall back to
WebGL2.

For repeated UI development, run the Trunk development server in a separate
terminal:

```sh
mix phx.server
cd ui
trunk serve
```

Then open <http://localhost:8080>. Requests under `/api` are proxied to Phoenix
on port 4000.

## Verify

```sh
mix test
cd ui
cargo check --target wasm32-unknown-unknown
```

# Elixir + GPUI Web collaborative editor

This demo is a browser-based collaborative editor. GPUI Web renders the Rust
UI to a WebGPU canvas, Yrs owns the browser CRDT, and Phoenix coordinates a
shared Yex document over Channels.

## Architecture

```text
Browser A: GPUI Web + Yrs ─┐
                           ├─ Phoenix Channel ─ Yex DocServer ─ Rustler NIF
Browser B: GPUI Web + Yrs ─┘
```

Rustler is used where it is useful: Yex uses a precompiled Rustler NIF to run
Yrs-compatible CRDT logic inside the BEAM. Browser Rust is compiled separately
to WebAssembly and does not use Rustler. Both sides use the y-sync v1 protocol,
so reconnects exchange only missing CRDT updates.

The shared document is currently kept in memory for the lifetime of the
Phoenix application. Authentication, durable persistence, and cursor awareness
are intentionally outside this demo's scope.

The Zed and [Guise](https://github.com/wess/guise) dependencies are pinned to
compatible revisions because GPUI Web is not yet published as a standalone
crate. Phoenix supplies the COOP and COEP headers required by GPUI Web.

## Requirements

- Elixir 1.17 or later and OTP 27 or later
- rustup and Rust nightly
- [Trunk](https://trunkrs.dev/) 0.21 or later
- A browser with WebGPU enabled; current Chrome or Edge is recommended

Install the browser UI toolchain once:

```sh
rustup toolchain install nightly --component rust-src --target wasm32-unknown-unknown
cargo install trunk --locked
```

Trunk is only the browser asset builder: it compiles the Rust entry point in
`assets/`, runs `wasm-bindgen`, and copies the generated JavaScript/WASM into
`priv/static`. The project-specific shell wrapper has been removed; the build
is now invoked directly by Mix aliases.

## Run

```sh
mix setup
mix assets.build
mix phx.server
```

Open <http://localhost:4000> in two tabs. Both tabs join the `demo` document;
edits in either tab are synchronized with the other.

For repeated UI development, run Phoenix and Trunk in separate terminals:

```sh
mix phx.server
mix assets.serve
```

Open <http://localhost:8080>. The UI uses the Phoenix server on port 4000 for
its Channel connection.

### Linux Chromium: `No available adapters`

Chromium on Linux may expose `navigator.gpu` while blocking every GPU adapter.
First enable graphics acceleration in the browser settings and restart it. For
local development, enable experimental WebGPU support if no adapter is still
available:

```sh
chromium --enable-unsafe-webgpu http://localhost:4000
```

Use `chrome://gpu` to confirm that WebGPU is hardware accelerated. GPUI Web
currently requires WebGPU and does not automatically fall back to WebGL2.

## Verify

```sh
mix precommit
mix assets.build
```

#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Some distributions install a system Rust ahead of rustup. GPUI Web needs the
# nightly toolchain and rust-src, so make that toolchain explicit when present.
if command -v rustup >/dev/null 2>&1; then
  nightly_rustc="$(rustup which --toolchain nightly rustc)"
  export PATH="$(dirname "$nightly_rustc"):$PATH"
fi

cd "$repo_root/ui"
exec trunk build --release

#!/usr/bin/env bash
#
# Build TuwaiqOS on Linux/macOS.
#
# Portable POSIX peer of scripts/build.ps1: it runs the exact same canonical
# three-stage build so local development and CI share one entrypoint.
#
# Output: target/debug/boot-bios-tuwaiqos.img
#
# The Rust toolchain is pinned by rust-toolchain.toml -- this script does not
# select or override it.

set -euo pipefail

# Resolve the repository root from the script's own location so this works no
# matter which directory it is invoked from.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${PROJECT_ROOT}"

# The build architecture expects every artifact in the shared root target
# directory: userland ELFs (step 1) are read back by build.rs (step 3), and the
# kernel ELF (step 2) is wrapped into the image (step 3).
export CARGO_TARGET_DIR="${PROJECT_ROOT}/target"

echo "== [1/3] userland ELF programs =="
# Standalone crate with its own [workspace] and .cargo/config.toml (static
# relocation / large code model / --no-pie). Build it from inside its own
# directory so cargo discovers that config; it must precede the image build.
(
  cd userland/hello
  cargo build --release
)

echo "== [2/3] bare-metal kernel =="
cargo build --package kernel --target x86_64-unknown-none

echo "== [3/3] BIOS disk image =="
cargo build --package tuwaiqos

IMAGE="${PROJECT_ROOT}/target/debug/boot-bios-tuwaiqos.img"
if [[ ! -f "${IMAGE}" ]]; then
  echo "ERROR: expected disk image was not produced: ${IMAGE}" >&2
  exit 1
fi

echo "OK: ${IMAGE} ($(wc -c < "${IMAGE}") bytes)"

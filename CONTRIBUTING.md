# Contributing to TuwaiqOS

Thank you for your interest in TuwaiqOS!

## Getting started

1. Install Rust nightly (`nightly-2026-06-01`) and QEMU.
2. Clone the repository and run `.\scripts\build.ps1`.
3. Test changes with `.\scripts\run-qemu.ps1`.

## Code style

- Keep kernel code `no_std` and beginner-readable.
- Add comments for non-obvious hardware or filesystem logic.
- Match existing module naming and error style (`Result<T, &'static str>`).
- Minimize scope — one feature per change when possible.

## What to work on

See [ROADMAP.md](ROADMAP.md). Good first issues:

- Host-side VFS path and corrupt-volume parser tests
- A second read-only VFS backend after the mount-table contract lands
- A selected physical NIC driver following the HAL claim/reset/DMA contract
- Unit tests for TuwaiqFS serialization (host-side)

## Pull requests

1. Describe what changed and why.
2. Confirm the validation checklist in README passes in QEMU.
3. Do not rewrite bootloader/framebuffer/keyboard unless the issue requires it.
4. Never hold the driver registry or another global lock across port/MMIO I/O,
   DMA completion, allocation, or scheduler sleeps. New drivers must document
   IRQ, DMA, register, failure, and teardown ownership.

## Reporting bugs

Include: QEMU version, host OS, steps to reproduce, expected vs actual output.

## License

By contributing, you agree that your contributions will be licensed under the MIT License.

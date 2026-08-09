# Security Policy

## Supported Versions

TuwaiqOS is currently in active development.

| Version / Branch | Supported |
| ---------------- | --------- |
| `main` (latest)  | ✅ |
| Older commits and releases | ❌ |

## Reporting a Vulnerability

Please do **not** report security vulnerabilities through public GitHub Issues.

Use GitHub Private Vulnerability Reporting:

https://github.com/italamrii/TuwaiqOS/security/advisories/new

Include:

- A clear description of the issue
- Affected component or commit
- Steps to reproduce
- Expected impact
- Logs, screenshots, or a proof of concept where relevant

We aim to acknowledge valid reports within 7 days.

Please allow reasonable time for investigation and a fix before public disclosure.

## Scope

Security reports are welcome for the kernel, memory management, interrupts,
scheduler, filesystem/VFS, userspace ABI, process isolation, drivers, build
pipeline, release artifacts, and Tuwaiq AI provider/permission boundaries.

Tuwaiq AI components are userspace components. A model or provider must never
receive an undocumented privileged path, unrestricted kernel authority, or a
permission bypass; reports of such a path are in scope even during preview
development.

Ring 3 filesystem mutation is currently limited to the installed
application's `/data/<process-name>/` namespace. User descriptors, paths, and
complete buffers are validated and copied before mutation. Applications cannot
replace `/apps`, write another application's data directory, or access backend
filesystem nodes directly. Shared/delegated access remains future capability
work rather than an implicit privilege.

TuwaiqFS commits mutations through alternating checksummed checkpoints. A bad
newest generation falls back only to an older valid committed generation. If
neither checkpoint is valid, the writable root remains offline in recovery
mode; corruption must never trigger an empty writable replacement or an
automatic reformat. The independent FAT32 `/boot` backend is read-only.

Phase 7 network resources are kernel-owned but process-scoped. UDP handles
contain a slot generation and are bound to the creating pid; a peer or stale
handle is rejected. System ports below 1024 and the kernel's ephemeral range
at 49152 and above are reserved for the future capability system. Send
descriptors and payloads are copied and bounded before enqueue;
the entire fixed receive destination is writable-user validated before a
packet is removed, including when the queue is empty. Process exit/kill closes
all owned sockets. VirtIO DMA uses fixed validated buffers and polling mode;
teardown resets the device before DMA scrub and ownership release. There is no
kernel HTTP, TLS, telemetry, compatibility, or AI network service.

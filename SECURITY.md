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
filesystem nodes directly. Phase 8 adds explicit shared access through
process-local, kernel-issued capabilities only. An owner may delegate no more
than its own rights and must select an exact normalized file/directory scope;
`..`, prefix confusion, alternate mount escape, `/boot` writes, `/apps`
replacement, forged/stale/cross-process handles, and rights amplification are
rejected. Revocation and owner exit invalidate descendants, while subsequent
operations fail with bounded ABI errors. Ring 3 still never receives a backend
node, disk-driver object, kernel pointer, or unrestricted VFS authority.

IPC ABI v1 messages are fixed at 304 bytes with at most 256 payload bytes.
Endpoints, queues, waiters, calls, process capability tables, grants, and VFS
scopes all have compile-time bounds. The kernel validates the complete ABI
structure and every nested user buffer before publication or VFS mutation.
Unknown versions/flags/types, noncanonical/kernel/unmapped/cross-page pointers,
overflow, table/queue exhaustion, duplicate/late/forged replies, and closed
peers fail deterministically. Blocking uses scheduler state and explicit wakeup,
not polling. Process exit closes owned endpoints, cancels calls, revokes grants,
removes waiters, releases queued messages, and frees capability state.

TuwaiqFS commits mutations through alternating checksummed checkpoints. A bad
newest generation falls back only to an older valid committed generation. If
neither checkpoint is valid, the writable root remains offline in recovery
mode; corruption must never trigger an empty writable replacement or an
automatic reformat. The independent FAT32 `/boot` backend is read-only.

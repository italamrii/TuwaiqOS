# TuwaiqOS Roadmap

> **Independent at the core. Compatible by design.**

This roadmap orders work by the dependencies needed to reach a genuinely
usable TuwaiqOS desktop. Phase numbers are integration and release gates, not a
requirement that all engineering happen serially: Core and Desktop work proceed
in parallel once the interfaces between them are clear.

`ARCHITECTURE.md` describes the current implementation. Unchecked items here
are plans, not claims about functionality that exists today.

## Architectural invariants

- The Tuwaiq Kernel remains an independent TuwaiqOS kernel. It will not be
  replaced by, derived from, or based on the Linux kernel.
- Native Tuwaiq applications and the versioned Tuwaiq ABI are first-class.
- POSIX and Linux compatibility are optional userspace layers. TuwaiqOS must
  remain fully bootable and functional when those layers are absent.
- Linux compatibility must be removable without breaking native TuwaiqOS.
- Core and Desktop development proceed in parallel; visible desktop progress
  does not wait for every Core phase to finish.
- AI models, providers, agents, and orchestration remain outside Ring 0. The
  kernel supplies narrow security and resource-control mechanisms, not AI
  policy or model execution.
- Security, testing, documentation, and measurable resource ownership are
  continuous requirements, even where a later phase contains a formal
  qualification gate.
- After the Phase 7 exit gate passes, feature development in Ring 0 is frozen.
  Later kernel changes are limited to security fixes, correctness fixes, and
  maintenance required for already-defined driver interfaces. Application,
  SDK, compatibility, package, service, and AI policy belongs in userspace.

## Dependency path

```text
Completed foundations (Phases 0-5)
        |
        v
Phase 6: Storage, VFS & Real Applications
        |
        v
Phase 7: Hardware & Networking
        |
        v
Phase 8: Application Platform
        |
        v
Phase 9: Applications, Packages & Updates
        |
        v
Phase 10: Optional Compatibility
        |
        v
Phase 11: Tuwaiq AI / Agentic OS
        |
        v
Phase 12: Security, Qualification & Government Pilot

Parallel Desktop Track -----------------------> Phases 6-9
Early Tuwaiq AI Preview ----> Phase 8 IPC/capabilities
                           ----> Phase 9 apps/tools
                           ----> Phase 11 full Agent Runtime
```

The numbered path is the order in which each integrated phase must meet its
exit gate. Hardware enablement, security work, SDK design, and Desktop work may
start earlier where they do not depend on an unstable interface.

## Completed foundations

### Phase 0 — Bootable foundation (completed)

- [x] BIOS bootloader, Rust `no_std` kernel, framebuffer/VGA console
- [x] PS/2 keyboard, interactive shell, history, and completion
- [x] Kernel heap and TuwaiqFS v2 persistent tree filesystem
- [x] Cooperative task foundation and built-in programs
- [x] Loopback networking foundation and offline AI Bridge stub
- [x] Built-in `notes`, `editor`, and `monitor` applications

### Phase 1 — Interrupt architecture (completed)

- [x] GDT/TSS and IDT with exception handling
- [x] PIC remapping, PIT at 100 Hz, interrupt-driven keyboard, and real uptime
- [x] Serial diagnostics and live interrupt/fault verification in QEMU

### Phase 2 — Physical memory and paging (completed)

- [x] Boot-memory-map-backed physical frame allocator with frame reuse
- [x] `OffsetPageTable` paging and a heap backed by mapped physical frames
- [x] Live heap/frame diagnostics and interrupt-safe paging locks

### Phase 3 — Preemptive scheduling (completed)

- [x] Real task control blocks, per-task kernel stacks, and context switching
- [x] 50 ms timer preemption, yield, sleep/wake, exit, task inspection, and kill
- [x] Concurrent shell, idle, and heartbeat tasks verified live in QEMU
- [x] Interrupt-safe scheduler, heap, keyboard, and paging lock discipline

### Phase 4 — Ring 3 process foundation (completed)

- [x] Hardware Ring 0/Ring 3 separation with a runtime TSS RSP0
- [x] Preempted user processes in private page-table roots
- [x] Checked ELF64 `ET_EXEC` loading with RX/RW/NX segment permissions
- [x] Initial syscall ABI and complete caller-address-space pointer validation
- [x] User-fault isolation, concurrent-process isolation, and spawn rollback
- [x] QEMU verification of CPL=3, invalid syscalls/pointers, hostile faults,
      distinct CR3 roots, scheduler regression, and reboot persistence

The detailed Phase 4 design, limitations, and verification record remain in
`ARCHITECTURE.md` under **Phase 4: user-mode process model**.

### Phase 5 — Userland runtime and first Tuwaiq Desktop (completed)

- [x] Reaping of terminated tasks, address spaces, frames, and kernel stacks
- [x] Anonymous `MMAP`/`MUNMAP` with rollback and atomic full-range unmap
- [x] Validated atomic framebuffer presentation and display information ABI
- [x] Bounded PS/2 mouse driver and unified keyboard/mouse input events
- [x] Exclusive foreground input ownership between shell and Ring 3 desktop
- [x] First Ring 3 Tuwaiq Desktop with idle redraw suppression, windows,
      launcher, focus, z-order, drag, close, keyboard input, normal exit, and
      safe relaunch
- [x] Repository-local acceptance harness covering hostile pointers, CPU page
      permissions, VM rollback, display/input atomicity, mouse behavior,
      process concurrency, 20 desktop lifecycle cycles, resource reuse,
      scheduler regression, and genuine reboot persistence

Phase 5 was accepted and merged into `main`. Reproducible commands, measured
results, security invariants, and bounded limitations remain in
`ARCHITECTURE.md` under **Phase 5 Verification Performed**. Completion of Phase
5 did not imply completion of the storage, networking, SDK, package, or
compatibility work below.

## Phase 6 — Storage, VFS & Real Applications (completed)

**Purpose:** replace build-time embedding as the normal application path and
give native userspace programs durable, path-based storage.

**Depends on:** the Phase 4 process/ELF foundation and Phase 5 resource
lifecycle.

- [x] Introduce the VFS facade/backend contract and mount TuwaiqFS at `/`
- [x] Add a general longest-prefix mount table and a genuinely independent
      read-only FAT32 backend at `/boot`
- [x] Define normalized absolute/relative path semantics, working directories,
      `cd`, and path-aware shell completion
- [x] Add the bounded read-only file syscall foundation (`CHDIR`, `GETCWD`,
      `OPEN`, `READ`, `CLOSE`) with validated buffers, per-process handles,
      stable offsets, and exit cleanup
- [x] Add bounded application-private file create/replace/delete, basic
      metadata, and directory create/list APIs with hostile-pointer coverage
- [x] Add bounded absolute seek on process-owned read handles
- [x] Execute native ELF binaries from files through the VFS with `SPAWN` and
      the existing validated ELF loader
- [x] Remove build-time embedded ELF as the normal application path; retain
      only explicitly justified boot, recovery, or test fixtures
- [x] Package and normally launch the Desktop, File Manager, Terminal, and AI
      Preview from `/apps` through the VFS and validated ELF loader
- [x] Add validated FAT32 read support as a separate VFS backend
- [x] Test binary-file persistence, path traversal boundaries, process resource
      reuse, and filesystem-backed ELF relaunch after a genuine reboot
- [x] Persist data from both a Ring 3 application and the built-in Notes app,
      then reopen both after a genuine reboot
- [x] Complete injected interrupted-write, corrupt-volume, automatic recovery,
      fail-closed recovery-mode, and storage-exhaustion acceptance tests

Shared/delegated filesystem authority is intentionally a Phase 8 dependency,
not a Phase 6 shortcut: it requires the versioned IPC and capability model
before applications can safely delegate access. A user-facing offline repair
utility is moved to Phase 9, after versioned package/system tooling exists;
Phase 6 still requires and verifies automatic checkpoint recovery plus
fail-closed behavior when no valid generation remains.

**Exit gate:** after a clean boot, the shell and desktop can discover, launch,
read, write, and relaunch native applications and their files from persistent
storage without rebuilding the kernel image. Recovery tests must not silently
accept corruption or data loss.

**Status:** completed and acceptance-tested. The shell and desktop discover
the prepackaged `/apps` catalog, launch useful native applications, persist
application-private data, genuinely reboot, reopen the same data, and relaunch
without rebuilding the tested image. The Phase 6 verification record and
reproducible harness are documented in `ARCHITECTURE.md`.

### Early Tuwaiq AI Preview Track — begins alongside Phase 6

This parallel preview exposes real architecture early without displacing the
Phase 6 dependency path. Phase 11 remains the completion target for the full
Agent Runtime and agentic operating-system experience.

- [x] Run the preview assistant service as an ordinary isolated Ring 3 process
- [x] Define a replaceable `ModelProvider` lifecycle and a local development
      provider that reports inference unavailable instead of fabricating it
- [x] Add a real **Tuwaiq AI — Preview** desktop launcher/window and prove
      launch, exit, relaunch, provider-fault isolation, and resource reuse
- [x] Keep telemetry, networking, capabilities, tools, and privileged access
      disabled in the preview
- [ ] Add a real local model provider only after filesystem/runtime primitives
      can load and execute it within explicit memory/resource limits
- [ ] Integrate service IPC and the Permission Broker in Phase 8
- [ ] Package assistant surfaces and permissioned tools in Phase 9
- [ ] Complete the Agent Runtime, audit log, workflows, and sovereign provider
      architecture in Phase 11

These four unchecked preview items are explicitly owned by Phases 8, 9, and 11
and are not Phase 6 exit requirements. Phase 6 does not add inference,
networking, telemetry, privileged actions, or a model-to-kernel path.

The preview security direction is fixed even while mechanisms remain future:
**Model proposes → Agent Runtime requests → policy checks → permission checks
→ tool executes → audit records.** A model never receives a direct privileged
kernel path.

## Phase 7 — Hardware & Networking

**Purpose:** replace single-machine assumptions with discoverable hardware and
usable networking while keeping deterministic virtual-hardware development.

**Depends on:** stable resource/file interfaces from Phase 6 where drivers or
network configuration persist state.

- [x] Define a HAL and modular driver framework with explicit device, IRQ,
      DMA, memory, and teardown ownership
- [x] Add bounded PCI configuration-space enumeration and immutable device
      discovery
- [x] Add legacy PCI VirtIO block and network transports for deterministic
      QEMU development, with fixed DMA, polling ownership, reset, and scrub
- [ ] Qualify a selected physical NIC through the same ownership interfaces;
      `virtio-net` is implemented, but successful emulation is not evidence of
      physical-hardware support
- [x] Build usable Ethernet link, IPv4, DHCP, DNS, and an owner-bound bounded
      UDP syscall surface
- [x] Begin the Tuwaiq Hardware Compatibility Program (THCP) with explicit
      profile definitions and a reproducible virtual reference procedure

Secure transport is preserved as required platform work but deliberately moves
to Phase 8's userspace runtime/SDK: TLS certificate, key, and protocol policy is
not a hardware primitive and would violate the post-Phase-7 kernel freeze if
implemented as a Ring 0 service. Hypervisor-specific VirtualBox/VMware drivers
are likewise not a Phase 7 exit requirement where their supported VirtIO
devices satisfy the same interface; any future device-specific driver is
ordinary THCP driver maintenance, not a new kernel subsystem.

### Tuwaiq Hardware Compatibility Program (THCP)

- **Tuwaiq Lite:** low-resource reference machine
- **Tuwaiq Standard:** mainstream reference machine
- **Tuwaiq Pro:** workstation/high-performance reference machine
- One TuwaiqOS image serves all profiles. Runtime capability detection selects
  appropriate features and defaults; these are not three operating-system
  forks.
- Driver and HAL interfaces must allow additional architectures, buses, and
  devices without redesigning the kernel or invalidating existing profiles.
- Qualification publishes explicit supported/unsupported capabilities rather
  than inferring support from a successful boot alone.
- The live support matrix and exact QEMU qualification command are maintained
  in `docs/HARDWARE_SUPPORT.md`.

**Exit gate:** deterministic virtual networking and selected real hardware can
obtain configuration through DHCP, resolve DNS, exchange traffic reliably,
recover from device errors, and pass isolation/resource-lifecycle tests. The
initial THCP matrix and reproducible qualification procedure exist.

**Current gate status:** open. The deterministic QEMU path and initial THCP
matrix are implemented and pass focused tests. A selected physical NIC and
physical-machine qualification have not been executed, so the Phase 7 kernel
freeze is not yet in effect and Phase 7 must not be marked complete.

## Parallel Desktop Track — Phases 6–9

Desktop work proceeds alongside Core work and integrates stable interfaces as
they land. It must not be held until every Core phase is complete.

- [ ] Replace polling-oriented paths with event-driven rendering and waiting
- [ ] Add dirty rectangles and compositor/presentation performance work
- [ ] Implement correct `KeyUp` events and richer input/focus semantics
- [ ] Add Arabic and English fonts, text shaping, bidirectional text, and RTL
      layout support
- [ ] Improve the window manager: resize, minimize, focus policy, workspace
      behavior, recovery, and accessibility foundations
- [ ] Evolve the launcher into a dock/application launcher
- [x] Build a filesystem-backed File Manager during Phase 6
- [x] Build a native Terminal on the Phase 6 process/file interfaces
- [ ] Build Settings as hardware, security, account, and capability surfaces
      become available
- [ ] Add notifications and clipboard services on the Phase 8 IPC/capability
      model
- [ ] Build a System Monitor using bounded diagnostics interfaces
- [ ] Integrate package/update discovery and status into the desktop during
      Phase 9

Each visible control must have real behavior. A placeholder must be identified
as such and must not imply an unsupported capability.

## Phase 8 — Application Platform

**Purpose:** make native TuwaiqOS applications stable, secure, portable, and
practical to develop.

**Depends on:** filesystem-backed applications from Phase 6 and the device/
network foundations needed by platform services from Phase 7.

- [ ] Stabilize and version the native Tuwaiq ABI with compatibility policy
- [ ] Provide userspace secure-transport primitives and certificate/key policy
      over the Phase 7 bounded socket ABI; TLS does not execute in Ring 0
- [ ] Add IPC with explicit endpoint ownership, bounds, and teardown behavior
- [ ] Add permissions/capabilities and least-privilege process services
- [ ] Move Tuwaiq AI Preview service/UI communication onto bounded IPC and
      require the same capabilities as every other native application
- [ ] Expand process services, including runtime memory management beyond the
      current bump-only `mmap` virtual-address arena
- [ ] Implement Tuwaiq libc and a useful native POSIX subset
- [ ] Provide Rust, C, and C++ SDKs, headers, libraries, examples, debuggers,
      profilers, and developer tooling
- [ ] Add native relocation/dynamic-library support only under a versioned ABI
      and when real applications require it
- [ ] Progress toward a self-hosting toolchain without making self-hosting a
      prerequisite for earlier application work
- [ ] Add multi-user/session foundations only after permissions and service
      isolation are enforceable

### TuwaiqOS Owner / Developer Mode

Owner / Developer Mode is a **local device role/capability**, enforced through
the same auditable permission model as other privileged operations. It may
provide:

- advanced diagnostics and kernel/driver/system monitoring;
- local crash and debug logs;
- package and repository controls;
- model and agent controls;
- hardware testing;
- release-channel selection and local build-signing tools.

It must never create a founder master key, universal remote access, hidden
privilege, undocumented bypass, or backdoor. Remote administration, if later
implemented, requires explicit device-local enrollment, revocation, audit, and
normal capability checks.

**Exit gate:** native sample applications built with supported SDKs run against
a versioned ABI, communicate through isolated IPC, receive only declared
capabilities, and survive service/process restart without leaked authority or
resources.

## Phase 9 — Applications, Packages & Updates

**Purpose:** turn the native platform into a maintainable application ecosystem.

**Depends on:** the versioned ABI, permissions, IPC, SDK, and persistent VFS.

- [ ] Deliver useful native applications through filesystem-backed execution
- [ ] Define a versioned package format and repository metadata
- [ ] Implement a package manager and deterministic dependency handling
- [ ] Require cryptographic package signing and verified provenance metadata
- [ ] Implement secure, transactional OS and application updates
- [ ] Support rollback, interrupted-update recovery, and storage-pressure cases
- [ ] Provide a user-facing offline TuwaiqFS inspection/repair utility using
      versioned system-tool and authorization interfaces; Phase 6 already
      provides automatic dual-checkpoint recovery and fail-closed detection
- [ ] Expose bounded package/repository controls through Settings and local
      Owner / Developer Mode
- [ ] Package the Tuwaiq AI application, local providers, and permissioned tool
      adapters independently so providers remain replaceable

**Exit gate:** signed native packages install, upgrade, remove, and roll back
without breaking unrelated applications or the bootable OS. Dependency,
signature, interruption, recovery, and downgrade-policy tests pass.

## Phase 10 — Compatibility

**Purpose:** broaden the software available to users without replacing the
native platform or changing the kernel's identity.

**Depends on:** stable native ABI, VFS, IPC, permissions, networking, packages,
and update/recovery mechanisms.

- [ ] Expand the useful userspace POSIX surface
- [ ] Implement Linux ABI compatibility entirely in userspace
- [ ] Start with carefully scoped static Linux ELF compatibility
- [ ] Add Linux dynamic linking, threads, signals, sockets, futex, and epoll
      only as required and with explicit security/resource limits
- [ ] Add a sandboxed WASM runtime
- [ ] Keep Win32 compatibility as long-term research only
- [ ] Test that compatibility components can be removed from an installed
      image without breaking boot or native TuwaiqOS applications

Linux compatibility is optional and removable. TuwaiqOS remains fully usable
with native applications when it is not installed.

**Exit gate:** selected compatibility workloads run inside userspace sandboxes,
cannot bypass native capabilities, and cannot destabilize native applications
or the kernel. Removing all compatibility packages leaves a bootable,
functional native system.

## Phase 11 — Tuwaiq AI / Agentic OS

**Purpose:** add sovereign, permissioned automation as replaceable userspace
services after the application and security foundations can constrain them.

**Depends on:** capabilities, IPC, native applications, packages/updates, audit
storage, and—only for optional remote providers—usable networking.

- [ ] Build offline-first local AI that remains useful without network access
- [ ] Define a replaceable model-provider architecture so a future suitable
      Saudi model can be adopted without redesigning the OS
- [ ] Build the Tuwaiq AI assistant as an unprivileged userspace application
- [ ] Separate the Agent Runtime, tools, policy, and audit system from the model
- [ ] Support permissioned document, file, PDF, and spreadsheet automation
- [ ] Support bounded system troubleshooting through diagnostic capabilities
- [ ] Add scheduled tasks with explicit owners, limits, review, and revocation
- [ ] Add optional email, calendar, and business connectors
- [ ] Require explicit capability permissions with allow-once and separately
      revocable persistent-workflow authorization
- [ ] Maintain a complete, user-visible audit log of actions and data access
- [ ] Deny unrestricted kernel/root access; agents receive narrow capabilities
      like every other application
- [ ] Allow optional cloud AI only by policy. Sovereign, offline, and
      air-gapped operation must remain possible
- [ ] Add policy-controlled userspace providers/connectors after userspace
      secure transport and permissions exist; no HTTP client belongs in Ring 0

No model or Agent Runtime component executes in Ring 0.
The Phase 6 preview is an architectural foothold, not completion of this
phase: real inference, the Agent Runtime, Permission Broker, tools, persistent
workflow authorization, and audit remain unchecked until demonstrated.

**Exit gate:** offline workflows function without cloud services; every action
is attributable, permission-checked, bounded, and revocable; provider
replacement does not change kernel or application APIs; hostile prompt/content
tests cannot obtain undeclared capabilities.

## Phase 12 — Security, Qualification & Government Pilot

**Purpose:** convert continuous hardening into a release-quality qualification
program suitable for controlled organizational deployment.

**Depends on:** the complete intended 1.0 scope and its update/recovery paths.

- [ ] Run continuous fuzzing of syscalls, parsers, protocols, filesystems,
      package metadata, compatibility loaders, drivers, and agent boundaries
- [ ] Harden syscall, parser, driver, DMA, and interrupt boundaries
- [ ] Produce secure, reproducible builds and a complete SBOM
- [ ] Establish signing-key generation, storage, rotation, revocation, and
      incident governance
- [ ] Qualify secure updates, rollback, disaster recovery, and factory recovery
- [ ] Qualify hardware across Tuwaiq Lite, Standard, and Pro THCP devices
- [ ] Run sustained stability, power-cycle, storage-fault, network-fault, and
      resource-exhaustion testing
- [ ] Complete independent penetration testing and remediate release blockers
- [ ] Prepare controlled organizational/government pilot operations,
      deployment, audit, support, and incident response

**Exit gate:** release artifacts are reproducible and signed; SBOM and key
governance are operational; update/recovery and THCP qualification pass;
sustained stability and penetration testing leave no unresolved release
blockers; pilot participation is controlled, auditable, and revocable.

## Release gates

Release names are evidence gates, not calendar promises or claims about the
current build.

### Technical Preview

- Core boot, shell, isolation, storage experiments, and desktop demonstrations
  run reproducibly on a documented virtual reference machine.
- Known destructive limitations are explicit; recovery and evidence collection
  are reproducible.

### Developer Preview

- Filesystem-backed native applications, early VFS/SDK contracts, and a usable
  desktop development loop are available on documented reference targets.
- Interfaces may still change, but changes and migrations are versioned.

### Alpha

- The intended 1.0 feature set is substantially integrated across native
  applications, hardware/networking, permissions, packages, and updates.
- Daily-use testing begins; known gaps are documented and no unsupported
  feature is presented as complete.

### Beta

- The intended 1.0 scope is feature-complete and API-frozen except for fixes.
- THCP qualification, upgrade/recovery testing, security review, accessibility,
  localization, and sustained stability meet published beta thresholds.

### Release Candidate (RC)

- No unresolved Blocker or High security/correctness findings.
- Reproducible signed images, SBOM, clean install, upgrade, rollback, recovery,
  penetration testing, and qualified hardware matrices satisfy release policy.

### 1.0

- An RC has sustained all release gates for the required observation period.
- Support, incident response, signing governance, update service, documentation,
  and controlled deployment processes are operational.

## Historical note

The project began as AbdullahOS, a learning operating system. It was renamed to
TuwaiqOS at v0.5 for public release. The historical version labels are retained
in Git history; this roadmap uses dependency-based phases from the completed
Phase 0–5 foundation onward.

# TuwaiqOS VMware Phase 7B Artifact

Generated virtual hardware version: `{{VIRTUAL_HW_VERSION}}`.

## What to give an external tester

For a focused Workstation / ESXi BIOS+NVMe boot check, only this file is
required:

- `{{PRIMARY_VMDK}}` — single-file `monolithicSparse` VMDK

Do **not** require the tester to edit a descriptor, import an OVA, or open a
VMX. SHA-256 checksums verify file integrity after transfer; they do **not**
prove boot compatibility.

### Attach on VMware Workstation (qualified attachment path)

1. Create a new VM: guest OS **Other 64-bit**, firmware **Legacy BIOS**
   (not UEFI), **1 vCPU**, **512 MiB RAM**.
2. Remove any default SCSI/SATA disk if present.
3. Add a hard disk → **NVMe** → Use an existing virtual disk → select
   `{{PRIMARY_VMDK}}`.
4. Optional but recommended: add a serial port, output to file
   `TuwaiqOS-VMware-BIOS-COM1.log` (COM1).
5. Keep networking disabled or unused for this milestone. Missing VirtIO /
   physical NICs must not block boot.
6. Secure Boot: off. Boot order: hard disk first.
7. Power on. If boot fails, return only:
   - the last visible on-screen boot message; and
   - the COM1 log file, if serial capture was enabled.

### Expected COM1 stage markers

A successful path reaches a shell or recovery console and includes:

```text
BOOT: stage=entry COM1-ready
BOOT: stage=kernel-entry
...
BOOT: stage=shell-entered
```

Optional devices report explicit offline/unavailable states instead of panic:

| Optional component | Non-fatal marker |
|---|---|
| NVMe rejected / absent | `storage: NVMe rejected safely:` or ATA fallback / no boot disk |
| VirtIO network absent | `virtio-net: no supported legacy PCI device; hardware network offline` |
| FAT32 `/boot` | `vfs: FAT32 /boot mount failed:` or `UNAVAILABLE` |
| Writable TuwaiqFS root | `vfs: TuwaiqFS unavailable; entering read-only recovery mode:` |
| Framebuffer | `framebuffer: rejected safely:` then VGA or serial console |
| PS/2 keyboard/mouse | controller absent / IRQ masked; boot continues |

Continuous `task heartbeat` lines mean the scheduler is alive; they are **not**
by themselves proof of interactive shell readiness. Prefer
`BOOT: stage=shell-entered` plus a prompt or recovery banner.

## Release-engineering companions

These are produced by `scripts/build-esxi.ps1` but are optional for the tester:

| File | Role |
|---|---|
| `TuwaiqOS-VMware-BIOS.vmx` | Example BIOS/NVMe/COM1 template |
| `SHA256SUMS.txt` | Transfer integrity only |
| `README-VMware.md` | Generated copy of this document |
| `{{TRANSPORT_VMDK}}` | `streamOptimized` transport image |

### ESXi note about streamOptimized transport

`{{TRANSPORT_VMDK}}` is for datastore upload / conversion workflows. It is
**not** the preferred direct-attach Workstation disk. If ESXi requires
conversion, run `vmkfstools` inside the **ESXi Shell/SSH** environment, for
example:

```text
vmkfstools -i {{TRANSPORT_VMDK}} TuwaiqOS-ESXi-Native.vmdk
```

Do not run `vmkfstools` from Windows PowerShell. Prefer attaching
`{{PRIMARY_VMDK}}` when the datastore accepts monolithicSparse directly.

## Build command

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-esxi.ps1 -VirtualHardwareVersion 13
```

The script:

1. builds the normal BIOS raw IMG (unless `-SkipBuild`);
2. validates the source IMG with `qemu-img info`;
3. detects `qemu-img` VMDK capabilities instead of assuming them;
4. writes `{{PRIMARY_VMDK}}` as `monolithicSparse`;
5. optionally writes `{{TRANSPORT_VMDK}}` as `streamOptimized`;
6. runs `qemu-img info` and `qemu-img check`;
7. emits VMX, checksums, and this README.

## Validation status

Artifact conversion and QEMU NVMe verification do **not** prove VMware
Workstation or ESXi compatibility. Phase 7B remains open until an external
tester returns:

- hypervisor product and version (Workstation and/or ESXi build);
- firmware = Legacy BIOS;
- disk controller = NVMe (or note if a different controller was required);
- RAM / vCPU used;
- last visible boot message;
- complete COM1 serial log when available.

Do not claim UEFI, PXE, physical NVMe, physical NIC, or arbitrary-device
support from this artifact path.

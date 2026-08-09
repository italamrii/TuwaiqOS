# Tuwaiq Hardware Compatibility Program

TuwaiqOS uses one image with runtime device discovery. Tuwaiq Lite, Standard,
and Pro are capability/qualification profiles, not separate operating-system
forks. A successful boot is not enough to mark a capability supported.

## Current support matrix

| Capability | Tuwaiq Lite | Tuwaiq Standard | Tuwaiq Pro |
|---|---|---|---|
| x86_64 BIOS boot | Qualified in QEMU `pc` | Target, not yet qualified on physical hardware | Target, not yet qualified on physical hardware |
| Memory | 128 MiB QEMU reference | 512 MiB target | 2 GiB+ target |
| Display/input | QEMU VGA framebuffer, PS/2 keyboard/mouse | Physical GPU/input unqualified | Physical GPU/input unqualified |
| Persistent root | ATA PIO virtual disk | Selected physical ATA configuration unqualified | Modern physical storage unqualified |
| PCI discovery | QEMU `pc` inventory qualified | Conventional PCI config mechanism 1 only | PCIe/ECAM unqualified |
| VirtIO block | Legacy/transitional PCI, polling, read foundation qualified | Same virtual device only | Same virtual device only |
| Networking | Legacy/transitional `virtio-net-pci`, IPv4/DHCP/DNS/bounded UDP qualified | Physical NIC unqualified | Physical NIC unqualified |
| IRQ/DMA isolation | Fixed DMA, polling VirtIO; reset and scrub | MSI/MSI-X and IOMMU unqualified | MSI/MSI-X and IOMMU unqualified |

“Qualified” here refers to the exact QEMU acceptance procedure below. No
physical desktop, laptop, server, NIC, GPU, NVMe device, Wi-Fi adapter,
Bluetooth adapter, USB controller, UEFI-only platform, or ARM platform is
currently claimed supported.

## Reproducible virtual qualification

From a clean candidate commit with QEMU installed:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\phase7-smoke.ps1
```

The harness boots the TuwaiqOS image as the first IDE disk and attaches a
separate deterministic legacy VirtIO block disk plus a legacy VirtIO network
device. It asserts bounded PCI inventory, an exact block-sector marker read,
link and MAC, DHCP address/route/DNS, a live DNS exchange, retained network
state, hostile Ring 3 UDP pointer/handle rejection, process-exit cleanup, and
device reset/DMA scrub/claim release. Evidence is written beneath
`target/phase7-smoke/` and is not committed.

DNS response addresses may change because the QEMU user-network resolver
forwards to the host resolver; acceptance checks a valid A response rather
than a hard-coded address. DHCP addresses and the VirtIO block marker are
deterministic within the harness.

## Physical qualification required to close Phase 7

Select and document one maintainable physical Ethernet controller, implement
it through the existing HAL ownership contract, and run the same link,
DHCP/DNS/traffic, hostile-process, device-error, teardown, reboot, storage,
desktop, and scheduler regression gates on named hardware. Record firmware,
PCI IDs, RAM, display/input/storage devices, and negative/unsupported results.
Until that evidence exists, Phase 7 remains open and the post-Phase-7 kernel
feature freeze has not started.

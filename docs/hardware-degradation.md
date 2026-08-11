# Hardware degradation

`scripts/degradation-suite.ps1` boots TuwaiqOS on deliberately incomplete
machines and checks that missing or unsupported hardware degrades instead of
hanging

```powershell
pwsh scripts/degradation-suite.ps1
pwsh scripts/degradation-suite.ps1 -Scenario no-legacy-ata
```

For each machine the suite asks three questions

1 does the kernel still reach a scheduling state
2 does it say so by naming the missing device on serial
3 do operations that need the missing device fail cleanly rather than hang

Question 2 is the one worth stating plainly
A kernel that loses a device and carries on without mentioning it has not
handled the failure
It has hidden it
An operator looking at that machine has no way to tell a missing disk from an
empty one so the suite treats silent degradation as a failure even when
nothing crashed

## Scenarios

| Name | Machine | What it exercises |
|---|---|---|
| `baseline` | default | the reference every other row is compared against |
| `no-ps2` | `-machine pc,i8042=off` | keyboard and mouse controller absent |
| `no-legacy-ata` | `-machine q35` | storage moved off the legacy ATA ports so the disk driver finds nothing |
| `no-ps2-no-ata` | `-machine q35,i8042=off` | input and storage missing at the same time |
| `unsupported-devices` | `-device e1000 -device intel-hda` | hardware with no driver present on the bus |
| `no-display` | `-vga none` | no display adapter at all |

Removing the PS/2 controller also removes the only way to type
Those scenarios are marked non interactive and are observed from serial alone

## Relationship to existing coverage

One of these scenarios is not new

`scripts/phase5-acceptance.ps1:535` already boots with `-machine pc,i8042=off`
and asserts four things about it

```
mouse-absent bounded initialization
mouse-absent IRQ remains masked
mouse-absent has no false initialized state
mouse-absent scheduler remains live
```

The `no-ps2` row here uses the identical QEMU machine string and checks the
same behaviour
It is kept as the input side reference that the combined `no-ps2-no-ata` row is
compared against rather than removed
but it should be read as a restatement of coverage the project already has and
not as something this suite found

The genuinely new rows are `no-legacy-ata` `no-ps2-no-ata` `unsupported-devices`
and `no-display`

Storage disappearing at runtime had no coverage anywhere before this and
`no-display` is the row that found a real failure

## How a row is judged

In this order

1 a panic or a double fault is always a failure
2 zero bytes of serial output is a failure even though nothing crashed
   A machine that produces no output cannot be diagnosed by anyone
3 reaching a running state but never matching the scenario `Expect` pattern is
   a failure because that is silent degradation
4 a shell that does not answer `ProbeExpect` is a failure

Only a row that reaches a running state and names its own degradation passes

## Results at `24287c3`

| Machine | Kernel outcome | What it reported |
|---|---|---|
| Reference | shell reached | `vfs: mounted TuwaiqFS v3 at /` |
| No PS/2 | shell reached with 34 heartbeats | `mouse: PS/2 controller unavailable during auxiliary-port setup` then `mouse: IRQ12 remains masked; boot continues without mouse input` |
| No legacy ATA | shell reached with 48 heartbeats | `vfs: TuwaiqFS unavailable; entering read-only recovery mode: ata error` and `vfs: FAT32 /boot mount failed: ata error` |
| Neither | shell reached with 59 heartbeats | both of the above |
| Unsupported devices | shell reached normally | nothing needed since no bus is enumerated |
| No display adapter | nothing at all | nothing |

No panic and no double fault occurred anywhere in the matrix

The two interesting rows

**No legacy ATA** is the strongest result
The kernel loses its disk during boot and says so in one line that names the
cause then continues to a working shell where `uptime` and `sysinfo` still
answer and filesystem operations return

```
Filesystem error: no VFS mount for path
```

rather than blocking
That is degradation working as intended

**No display adapter** produces zero bytes of serial output over 100 seconds
with the process still running and no panic
The same image with `-vga std` produces 2914 bytes and reaches a shell

Not one byte appears including the bootloader own first line so the stall
happens at or before the bootloader serial initialisation which places it in
the pre kernel path
The exact stopping point could not be located because SeaBIOS in this QEMU
build emits no debug console output so that attribution is unconfirmed

## What this suite deliberately does not claim

The task asked for boot without NVMe and without network hardware

Neither can fail at this commit because neither is ever touched

There is no NVMe driver in `kernel/src/`
The only storage driver is `kernel/src/ata.rs` and NVMe support lives on the
unmerged `phase7b/esxi-nvme-compatibility` branch

The only implementation of the `NetDriver` trait in `kernel/src/net/` is
`loopback.rs`
No PCI enumeration and no hardware probing happens anywhere so `-net none`
changes nothing

Those two rows are absent from the matrix rather than present and green
A passing row for a driver that does not exist would be worse than no row

## Artifacts

```
target/degradation/<commit12>-<timestamp>/
  results.json          verdict counts and per-scenario rows with evidence
  manifest.json         run identity
  report.txt            the same thing but readable
  <scenario>/serial.log the full capture for that machine
```

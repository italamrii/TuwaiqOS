# Mixed-workload memory stress

`scripts/memory-stress.ps1` runs the existing memory exercises interleaved and
repeatedly and checks that the kernel returns to its baseline

```powershell
pwsh scripts/memory-stress.ps1
pwsh scripts/memory-stress.ps1 -Rounds 10
```

## What was missing

The project already measures memory behaviour thoroughly and each of these has
its own assertions in `scripts/phase5-acceptance.ps1`

| Command | What it covers |
|---|---|
| `spawnfail` | frames rolled back after a failed ELF load |
| `reap` | TCB and kernel stack reclamation over 20 normal exits |
| `autoreap` | the same through the grace period path |
| `killreap` | the same through an explicit kill |
| `mmap_exhaustion` | anonymous memory rollback and reuse |
| `isolate <fault>` | one CPL=3 fault of each kind contained |

Every one of them starts from a clean system and exercises a single activity

What none of them reaches is the case where those activities follow one
another A leak that only appears on the fault path or only when a faulted
process is followed by a successful spawn accumulates across a mixed workload
and is invisible to any single one of these

One round here is

```
isolate bad_ud2      a fault killed from inside an interrupt handler
isolate bad_kernel   a page fault on a kernel address
runelf hello         an ordinary process that exits normally
runelf bad_mmap      anonymous memory requested and rejected
runelf bad_pointer   a syscall given a pointer it must refuse
isolate bad_divzero  a divide error
```

followed by `reap` to clear the grace period and a measurement

## The metric

The frame bump cursor and not the allocation count

`allocated` counts every call to `allocate_frame` including the ones served
from the free list so it climbs steadily on a system that is recycling
perfectly `kernel/src/paging.rs` says exactly this at
`BootInfoFrameAllocator::frames_bumped` and `handle_spawnfail` repeats it at
length

The bump cursor only moves when a frame is taken that was never taken before
so it is the one number that answers whether anything leaked

The suite reads it through `spawnfail 1` which prints it as part of its own
report

**The first round is allowed to move it** A freshly booted system has never
had to satisfy this peak so it bumps fresh frames once to build up the pool
What is asserted is that it then stops This is why the suite needs at least
three rounds to say anything and reports `SKIP` rather than a false pass below
that

## Result at `24287c3`

```
point         bump cursor    heap used
baseline             1040
round 1              1051       151224
round 2              1051       151224
round 3              1051       151224
round 4              1051       151224

[PASS] no panic under mixed load - 12 CPL=3 faults trapped and contained across 4 rounds
[PASS] frame bump cursor settles - cursor stopped at 1051 and stayed there for 3 rounds
[PASS] kernel heap settles - unchanged after warm-up
```

The cursor moves 11 frames during the first round and then does not move again
The kernel heap is byte for byte identical across rounds 1 to 4

Twelve CPL=3 faults were trapped and contained with no panic

That is the signature of a system with no leak on any of these paths including
the fault path

## A note on what `sysinfo` shows

The same run through `sysinfo` looks alarming and is not

```
baseline   1073 frames allocated
round 1    1224 frames allocated
round 2    1375 frames allocated
round 3    1526 frames allocated
round 4    1677 frames allocated
```

A clean linear climb of about 150 frames per round on a system that is
provably not leaking

`sysinfo` reports `allocated` which double counts every reused frame The
number that matters is not reachable from `sysinfo` at all and has to be read
out of `spawnfail` which is a command for testing failed loads

An operator watching the obvious command sees unbounded growth on a healthy
machine

## Artifacts

```
target/memory-stress/<commit12>-<timestamp>/
  results.json   verdict counts and per-check evidence
  manifest.json  run identity
  serial.log     the full capture including every measurement
```

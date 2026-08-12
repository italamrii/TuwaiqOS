# Ring 3 fault isolation coverage

Where each requirement is tested and what was missing

## The requirements

Invalid syscall invalid pointer invalid handle unmapped access illegal
instruction and crash with relaunch with the offending program dying or
returning an error while the kernel and the shell carry on

## Where each one is already met

Every row below is existing coverage in `scripts/phase5-acceptance.ps1` and
none of it was rewritten

| Requirement | Program | Assertion | Line |
|---|---|---|---|
| Invalid syscall | `bad_syscall` | `security ELF: bad_syscall` | 363 |
| Invalid pointer | `bad_pointer` | `security ELF: bad_pointer` | 363 |
| Invalid handle | `file_api_test` | `SYS_READ` with handle `u64::MAX` refused | 106 of the program |
| Unmapped access | `bad_unmapped` | `Ring 3 fault isolation: bad_unmapped` | 345 |
| Illegal instruction | `bad_ud2` | `Ring 3 fault isolation: bad_ud2` | 345 |
| Privileged instruction | `bad_privileged` | `Ring 3 fault isolation: bad_privileged` | 345 |
| Divide error | `bad_divzero` | `Ring 3 fault isolation: bad_divzero` | 345 |
| Kernel address read | `bad_kernel` | `Ring 3 fault isolation: bad_kernel` | 345 |
| Kernel keeps running | all of the above | `Both finished:` with two exit codes of zero | 346 |
| Crash then recover | `desktopfaultpeer` | `forced desktop failure recovers shell/resources` | 51st assertion |
| Relaunch | `desktopcycle` | `20-cycle desktop restart` | 52nd assertion |
| Resources return | `desktopcycle` | TCB frame and heap baselines | 53rd to 55th |

The address space boundary itself is covered separately by `isolate` which
asserts distinct PML4 physical addresses per process

Sixty two assertions in that file in total This is a well tested area and
saying so is more useful than adding a second suite that repeats it

## What was missing

Repetition of the same fault

Every fault above is taken once The first fault of a kind exercises the
handler The twentieth exercises whether anything the handler leaves behind
accumulates and nothing measured that

The two relaunch assertions come closest and neither reaches it
`desktopcycle` restarts a program that exits normally twenty times and
`desktopfaultpeer` forces one failure once
Neither repeats a crash

That matters because a crash does not leave through the same door as a normal
exit A process killed from inside a fault handler abandons its stack rather
than unwinding it so anything the handler was holding is released on a
different path or not at all Twenty normal exits say nothing about twenty
crashes

## What `scripts/fault-relaunch.ps1` adds

Crashes the same program the same way twenty times and after every single one
asks

```
runelf bad_ud2     crash it
uptime             is the shell still there
```

Three things are checked per crash and one at the end

| Check | Why |
|---|---|
| the process exited with the code its fault implies | a fault handled differently the twentieth time is a bug even if nothing crashes |
| the shell answered the next command | this is the actual claim being tested and asking every time pins a failure to a crash number |
| the kernel never panicked | the obvious one |
| the frame bump cursor did not move after the first crash | a per crash leak of even one frame is visible over twenty and invisible over one |

Exit codes come from `kernel/src/interrupts.rs`

```
EXIT_CODE_SEGV = 139   page fault from CPL=3
EXIT_CODE_ILL  = 132   general protection fault or invalid opcode
EXIT_CODE_FPE  = 136   divide error
```

`-Program` selects which fault to repeat so the same run can be pointed at any
of the five

The cursor is read after the first crash rather than before it for the same
reason `scripts/memory-stress.ps1` does so A freshly booted system has never
had to satisfy this peak and is entitled to bump fresh frames once What is
asserted is that it then stops

## Result at `24287c3`

Twenty crashes of `bad_ud2`

```
[PASS] kernel survives every crash    - 20 crashes and 20 CPL=3 faults trapped with no panic
[PASS] every relaunch ran             - 20 of 20
[PASS] exit code stays consistent     - all 20 exited 132
[PASS] shell answers after every crash - answered all 20 times
[PASS] no frames leaked per crash     - cursor stayed at 1040 across 19 further crashes

verdict: PASS
```

The bump cursor did not move at all across the nineteen crashes after the
first The twentieth crash behaved exactly like the first and the shell
answered `uptime` after every one

## Artifacts

```
target/fault-relaunch/<commit12>-<timestamp>/
  results.json   verdict counts and per-check evidence
  manifest.json  run identity
  serial.log     every crash and every shell answer
```

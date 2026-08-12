# Guest isolation probes

Four Ring 3 programs that test what a user process can reach without going
through a syscall at all

```
runelf probe_descriptors   read the descriptor tables
runelf probe_msr           read a model specific register
runelf probe_portio        talk to the disk controller directly
runelf probe_leaked        dereference an address learned at runtime
```

## Scope

The task this was written for names NPT GPA MSR CPUID and VMEXITs which are
hypervisor terms

TuwaiqOS contains no virtualization code There is no VMX and no SVM anywhere in
`kernel/src` so it is a guest and never a host and there is no nested page
table for it to get wrong

What can be tested is the boundary TuwaiqOS actually owns which is the one
between Ring 3 and Ring 0 That is what these four probe and the reinterpretation
is stated here rather than left implied

## What they check

Three of the four are written to fail A correct kernel terminates them and the
message after the attempt never prints so a clean exit is the failure

`probe_descriptors` is the opposite It is written to succeed because it proves
a disclosure rather than a fault

| Probe | Attempts | Correct outcome |
|---|---|---|
| `probe_descriptors` | `sgdt` `sidt` `sldt` `str` | all four succeed which is the finding |
| `probe_msr` | `rdmsr` on IA32_EFER | general protection fault |
| `probe_portio` | `in al, 0x1F7` the ATA status port | general protection fault |
| `probe_leaked` | `sidt` then dereference the base it returns | page fault |

`probe_portio` is the one with teeth If port I/O were reachable from CPL=3 a
user program could drive the disk controller directly and read or write any
sector with the filesystem and every check above it bypassed

`probe_leaked` closes the loop on `probe_descriptors` A leaked address only
matters if it can be used and unlike `bad_kernel` which dereferences a constant
chosen when it was written this one learns the address from the machine at
runtime so a kernel that moved its tables would not change the result

## Results at `24287c3`

```
probe_descriptors: GDT base=0x0000010000568208 limit=0x000000000000002F
probe_descriptors: IDT base=0x00000100005621D0 limit=0x0000000000000FFF
probe_descriptors: LDTR=0x0000000000000000 TR=0x0000000000000010
probe_descriptors: LEAKED -- CR4.UMIP is clear so CPL=3 read all four
process: result pid=4 name=probe_descriptors state=Terminated exit_code=0
```

```
probe_msr: about to read IA32_EFER with rdmsr from CPL=3
EXCEPTION: GENERAL PROTECTION FAULT (error_code=0)
process: result pid=5 name=probe_msr state=Terminated exit_code=132
```

```
probe_portio: about to read the ATA status port from CPL=3
EXCEPTION: GENERAL PROTECTION FAULT (error_code=0)
process: result pid=6 name=probe_portio state=Terminated exit_code=132
```

```
probe_leaked: sidt gave IDT base=0x00000100005621D0
probe_leaked: about to dereference it from CPL=3
EXCEPTION: PAGE FAULT at VirtAddr(0x100005621d0)
error_code=PageFaultErrorCode(PROTECTION_VIOLATION | USER_MODE)
usermode: page fault trapped safely from CPL=3 -- terminating the offending
process, kernel continues
process: result pid=5 name=probe_leaked state=Terminated exit_code=139
```

Three boundaries hold and the shell answered normally after every one of them

## The finding

`CR4` is never written anywhere in `kernel/src` so `CR4.UMIP` is clear

`sgdt` `sidt` `sldt` and `str` are unprivileged on x86-64 The CPU refuses them
at CPL=3 only when UMIP is set so on this kernel any user program can read
where the GDT and the IDT live

It is a disclosure and not an escalation `probe_leaked` shows the address
cannot then be dereferenced so the paging boundary still holds

What it costs is address secrecy Any future mitigation that depends on a user
program not knowing where kernel structures are starts out defeated

`CR4.UMIP` is one bit and needs the CPU to support it which is Skylake and
later Setting it would close this and would also hide `sldt` and `str`

Two neighbouring bits are also clear and are worth naming in the same breath
even though neither is exploitable here

`CR4.SMEP` stops Ring 0 executing a page marked user accessible
`CR4.SMAP` stops Ring 0 reading or writing one without an explicit `stac`

Neither is load bearing today NX is enabled and the syscall layer validates
user pointers explicitly Both are the kind of defence that matters when a
mistake is eventually made rather than before

## Adding a probe

One file in `userland/hello/src/bin/` one arm in `embedded_program` in
`kernel/src/shell.rs` and one entry in the `$UserlandBins` list in
`scripts/build.ps1` so a missing binary fails the build rather than the test

A probe that must fail should print its result only after the attempt so the
message appearing at all is the failure

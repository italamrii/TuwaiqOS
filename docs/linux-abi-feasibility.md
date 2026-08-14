# Linux ABI compatibility what it would actually take

A measured sizing of the first unchecked item in Phase 10 rather than an
implementation of it

> Start with carefully scoped static Linux ELF compatibility

Nothing here changes how TuwaiqOS behaves The one code addition is a probe that
answers a question the source alone cannot settle

## What the roadmap requires

Four constraints from `ROADMAP.md` shape everything below

> The Tuwaiq Kernel remains an independent TuwaiqOS kernel It will not be
> replaced by derived from or based on the Linux kernel

> POSIX and Linux compatibility are optional userspace layers TuwaiqOS must
> remain fully bootable and functional when those layers are absent

> Linux compatibility must be removable without breaking native TuwaiqOS

> Implement Linux ABI compatibility entirely in userspace

Phase 10 also declares its dependencies

> Depends on stable native ABI VFS IPC permissions networking packages and
> update and recovery mechanisms

IPC is a draft on `phase8/ipc-capabilities` and packages and update and
recovery do not exist yet so this is a sizing exercise and not a proposal to
start building

## The first blocker is not a syscall it is the instruction

Every Linux x86-64 binary enters the kernel with the `syscall` instruction
TuwaiqOS enters with `int 0x80`

`syscall` only decodes when `EFER.SCE` is set `kernel/src/paging.rs:50` sets
`EFER.NO_EXECUTE_ENABLE` and nothing else and no code anywhere in `kernel/src`
writes `LSTAR` `STAR` or `SFMASK`

Measured rather than inferred `runelf probe_syscall_insn`

```
probe_syscall_insn: about to execute `syscall` from CPL=3
EXCEPTION: INVALID OPCODE
InterruptStackFrame {
    instruction_pointer: VirtAddr(0x70000000102f),
    code_segment: 43,
```

The instruction does not decode

**This is the part that cannot live in userspace** An instruction that raises
`#UD` cannot be intercepted by anything running at CPL=3 There is no way for a
userspace layer to see the call at all

Three ways out and each is a Ring 0 change

| Approach | Ring 0 cost | Notes |
|---|---|---|
| Enable `EFER.SCE` and install a `syscall` entry point | one MSR write and an entry stub | the normal way and the smallest change |
| Catch `#UD` and decode the instruction in the handler | a decoder in the fault path | works with no MSR change but puts instruction decoding in Ring 0 which is worse |
| Translate the binary before running it | none in Ring 0 | a rewriter in the loader replacing every `syscall` with `int 0x80` keeps the kernel untouched and is genuinely userspace but breaks on any binary that computes addresses or checks its own bytes |

The third is the only one that satisfies the wording as written and it is also
the most fragile Worth deciding deliberately rather than discovering later

## How many syscalls a static binary actually needs

Measured by disassembling real static binaries and recovering the number
loaded into `RAX` at every `syscall` site The method over reports since a
number found this way may sit on a path never taken which is the safe
direction for sizing

| Binary | Size | Distinct syscalls reachable |
|---|---|---|
| Hand written assembly hello world | 8 840 bytes | **2** |
| glibc static hello world compiled with `gcc -static -O2` | 769 224 bytes | **44** |

The assembly binary needs `write` and `exit` and nothing else

The C binary needs 44 because glibc's startup runs long before `main` It sets
up thread local storage with `arch_prctl` reads limits with `prlimit64` seeds
its allocator with `brk` and `mmap` installs signal handlers with
`rt_sigaction` registers `set_tid_address` and `set_robust_list` for threading
it will never use and touches `rseq` `getrandom` and `clock_gettime`

That gap between 2 and 44 is the whole story of scoping this A layer that
targets hand written or freestanding binaries is small A layer that targets
anything a normal toolchain produces is not

## What TuwaiqOS already has

The native ABI is 22 calls numbered 0 to 21 in `kernel/src/syscall.rs`

Of the 44 that glibc reaches these map onto something that already exists

| Linux | TuwaiqOS | Fit |
|---|---|---|
| `write` | `WRITE` 1 | direct for stdout |
| `exit` and `exit_group` | `EXIT` 0 | direct |
| `getpid` | `GETPID` 3 | direct |
| `mmap` anonymous | `MMAP` 4 | close but TuwaiqOS takes `len` and `writable` and returns an address with no `prot` or `flags` or file backing |
| `munmap` | `MUNMAP` 5 | direct |
| `openat` | `OPEN` 12 | TuwaiqOS has no directory file descriptors so only the absolute path case maps |
| `read` | `READ` 13 | direct |
| `close` | `CLOSE` 14 | direct |
| `lseek` | `SEEK` 21 | direct |
| `getcwd` | `GETCWD` 11 | direct |
| `chdir` | `CHDIR` 10 | direct |
| `getdents64` | `READDIR` 19 | different record format |
| `fstat` and `newfstatat` | `STAT` 20 | TuwaiqOS stats by path not by handle |
| `clock_gettime` | `UPTIME_TICKS` 9 | ticks since boot and no wall clock exists |

Fourteen of the 44 have something behind them Two of those fourteen are only a
partial fit

The remaining thirty have nothing behind them at all `brk` `mprotect`
`arch_prctl` `rt_sigaction` `rt_sigprocmask` `ioctl` `writev` `fcntl` `futex`
`gettid` `set_tid_address` `set_robust_list` `rseq` `getrandom` `prlimit64`
and the rest

Several of those are not small `futex` is the foundation of every threading
primitive `rt_sigaction` needs a signal delivery mechanism the scheduler does
not have `mprotect` needs per page permission changes after mapping

## What a first step could reasonably be

Given the wording of the roadmap the honest minimum is narrow

Target static freestanding binaries and not glibc output Two syscalls covers
the assembly case and a handful more covers a small freestanding C program
built with `-nostdlib`

That is a real milestone It proves the loader accepts a Linux ELF the entry
convention works and the argument registers translate and it does so without
committing to signals threads or an allocator

It is also honest about what it is not It would not run anything from a normal
Linux distribution and saying otherwise would set an expectation the layer
cannot meet

The ordering that follows from the measurements

1 decide where the `syscall` instruction is handled since nothing can start
  until that is settled and it is the one decision that touches Ring 0
2 accept a Linux ELF in the loader and check the entry convention
3 translate the two calls the assembly case needs
4 measure again against a freestanding C binary rather than guessing what it
  needs
5 only then consider whether glibc is a goal at all

## Reproducing the measurements

The syscall extraction script and both test binaries are in the pull request
description rather than committed here since they are host tooling and not part
of TuwaiqOS

The instruction probe is committed and runs as

```
runelf probe_syscall_insn
```

It is written to fail A kernel that had wired up `syscall` would print

```
probe_syscall_insn: UNEXPECTED -- the instruction decoded
```

and exit 1 so the probe stays useful as a regression check after the decision
in step 1 is made

# Unified acceptance harness

`scripts/acceptance.ps1` runs the project's test suites, adds a liveness probe
of its own, and reduces everything to one verdict with evidence attached.

```powershell
pwsh scripts/acceptance.ps1                    # everything
pwsh scripts/acceptance.ps1 -SelfTest          # verify the detectors, boots nothing
pwsh scripts/acceptance.ps1 -LivenessOnly      # boot + scheduler check only
pwsh scripts/acceptance.ps1 -Suite phase6      # substring filter over suite names
```

## Why a unifier and not a replacement

The existing suites are the authority on their own phases and stay that way.
What was missing sits between and underneath them:

* **No single verdict.** Each suite reports separately, and most of them fall
  off the end without calling `exit`, so a green exit code means "the script
  finished", not "the checks passed".
* **Nothing watches for the failures a suite cannot report about itself.** A
  suite that hangs produces no result at all; a kernel that panics after a
  suite's last assertion still looks like a pass; a kernel that stops
  scheduling looks identical to one that is merely slow.

This harness supplies both, and does so without changing a line of the suites
it runs.

## Verdicts

| Verdict | Meaning | Exit |
|---|---|---|
| `PASS` | Every selected suite passed and the liveness probe was clean | 0 |
| `PARTIAL` | At least one passed, at least one skipped, none failed | 2 |
| `FAIL` | Any suite failed, or the probe found a panic, hang, or stall | 1 |
| `SKIP` | Nothing could run — no image, no QEMU, no suites selected | 3 |

`PARTIAL` exists so a run on a machine that cannot execute every suite is not
reported as a pass. Coverage that did not happen is never counted as coverage
that succeeded.

## How a suite's result is decided

Three sources, most authoritative first:

1. **`manifest.json` → `verdict`** — where the suites actually record their
   outcome. A verdict of `INCOMPLETE` or `DEVELOPMENT-SKIPPED` maps to `SKIP`,
   not `PASS`: a suite that declined to run its checks is a gap in coverage.
2. **`results.json`** — count rows whose `status` is not `PASS`.
3. **Exit code** — last resort, and only meaningful for the suites that set one.

### A suite that could not start is not a failing kernel

If a suite produced no verdict at all, the harness checks whether it ever
started. A suite that bailed out on the host environment — an unavailable
cmdlet, a missing image, a branch precondition — is recorded as `SKIP` with
the reason preserved, because reporting it as `FAIL` would attribute an
operator problem to the code under test.

The signatures for this are an explicit list, not a catch-all. Anything
unrecognised stays `FAIL`: laundering a real failure into a skip is far worse
than the reverse, so the default is pessimistic. The self-test asserts both
directions, including that an unknown error message never becomes a `SKIP`.

Suites do not share a parameter set — `qemu-smoke-test.ps1` has no `param()`
block at all and finds its own image. The harness inspects each suite's
declared parameters and passes only what that suite accepts, so registering a
new suite never requires changing its signature to fit.

Registering one is a single line in the `$Registry` table at the top of the
script. `Required = $true` means the suite's absence is a `FAIL` rather than a
`SKIP`.

## The liveness probe

Boots the image and answers three questions the suites cannot:

**Did it panic?** The kernel prints a delimited block; the probe lifts the
body out of it, so `results.json` carries the location and the message rather
than just the banner:

```
kernel panicked while running: panicked at kernel/src/main.rs:115:9: | negative control: induced kernel panic
```

Heartbeats continue after a panic — the panicking task dies but the scheduler
keeps running — so panic detection cannot be inferred from output stopping. It
is a text match, deliberately.

**A contained user fault is not a crash.** Since Phase 4, the page-fault, GPF,
invalid-opcode and divide-error handlers check the saved CS: an RPL of 3 means
a user program did something its own mappings forbid, so the kernel ends that
process and carries on. A Ring-0-origin fault takes the old path — a halt.

Both print the same `EXCEPTION: ...` line first, so the line alone cannot tell
them apart; only the `usermode: ... trapped safely from CPL=3` notice that
follows can. The probe compares, per fault kind, how many exceptions were
raised against how many were recovered, and treats only the unmatched ones as
fatal — which stays correct when several faults occur in one run.

This matters for anything testing Ring-3 isolation, where faulting a user
process on purpose is the whole point. Matching `EXCEPTION: PAGE FAULT` on
sight would report a correctly-working kernel as dead on every such test.
Contained faults are counted and surfaced as `ContainedUserFaults`, so a test
can assert it actually exercised the path it meant to.

**Did it reach a shell?** The shell banner is authoritative. The heartbeat is a
fallback for configurations where the shell talks only to the framebuffer: by
the third beat, the scheduler has demonstrably preempted and resumed a task,
which is the property that actually matters.

If QEMU exits before either marker, that is reported as a probable triple
fault rather than being left to time out as a hang — `-no-reboot` turns the
reset loop into a process exit.

**Is it still scheduling?** The `heartbeat` task sleeps `sleep_ticks(100)`
against a 100 Hz PIT, so it emits one line per second of *interrupt* time.
Counting those lines over a known wall-clock window measures how many timer
interrupts actually reached the CPU. A kernel that has masked interrupts,
deadlocked inside an ISR, or stopped scheduling shows a rate far below 1 Hz
while still looking "up" to every other check.

The 0.5 Hz floor is set from measurement, not taste:

| Case | Rate | Source |
|---|---|---|
| Healthy image | 20 beats / 20 s (1.00 Hz) | measured |
| Threshold | 10 beats / 20 s (0.50 Hz) | — |
| ELF-loader stall | 11 beats / 150 s (0.07 Hz) | measured, 93% of ticks lost |

The threshold sits an order of magnitude from both: it will not fire on
emulator jitter, and it cannot miss a real stall.

## Self-test

A detector that has never been shown a failure is not a detector.

`-SelfTest` runs the two classifiers against captured serial logs and against
the numeric boundary. It boots nothing, needs no image and no QEMU, requires
no clean worktree, and finishes in about a second — so it works as a CI
pre-flight before the expensive stages, and on a machine that cannot boot
anything at all.

The fixtures in `tests/fixtures/serial/` are real captures, not hand-written
text:

| Fixture | Origin |
|---|---|
| `boot-healthy.log` | A clean boot of the current tree |
| `panic-oom.log` | A recorded allocator-failure panic |
| `hang-no-shell.log` | A boot truncated inside the bootloader |

Twenty-three checks in total. Three of them exist because they caught real
defects while this was being written:

* The "has it reached the third beat" check was `beat #[3-9]\b`, which never
  matches `beat #97` — a word boundary cannot land inside a number. A capture
  that started late would have been read as never having scheduled.
* `EXCEPTION: PAGE FAULT` was treated as fatal on sight, which would have
  reported a correctly-contained Ring-3 fault as a kernel death.
* The suite start-failure signatures included `image not found` and
  `qemu.*not found`, loose enough to match an assertion like *"expected image
  not found on disk"* — and would then have downgraded a real failure to a
  skip. They are now anchored to the suites' own wording, and the self-test
  asserts that such assertions stay `FAIL`.

## End-to-end validation

The classifiers are checked against fixtures; the probe itself was checked
against deliberately broken kernels, because a harness that has only ever seen
a working system proves nothing.

| Injected fault | Harness output | Verdict |
|---|---|---|
| `panic!` after boot | `kernel panicked while running: panicked at kernel/src/main.rs:115:9: \| negative control: induced kernel panic` | `FAIL`, exit 1 |
| `interrupts::disable()` after boot — no panic, no exit, no output | `scheduler stalled: 7 heartbeats in 20s, expected >= 10` | `FAIL`, exit 1 |
| None (pristine tree) | `booted to shell, 20 heartbeats in 20s, no panic` | `PASS`, exit 0 |

The second row is the case that matters. That kernel never panicked, never
exited, and never printed an error — every other check in the repository would
have called it healthy.

Both faults were injected into a throwaway build and reverted; no kernel
source was modified.

## Artifacts

```
target/acceptance/<commit12>-<timestamp>/
  results.json          verdict, counts, per-suite rows with evidence
  manifest.json         run identity
  report.txt            the same thing, readable
  liveness/serial.log   the probe's capture
  <suite>/              each suite's own output, unchanged
  <suite>.log           each suite's console output
```

Following the layout `phase6-storage-smoke.ps1` established, so a reader who
knows one run knows all of them.

# Host-side filesystem tests

Runs `kernel/src/tuwaiqfs.rs` and `kernel/src/fs.rs` on the host, compiled
verbatim, and attacks them with damaged metadata and a failing disk

```console
cd tests/tuwaiqfs-host
cargo test
```

No QEMU and no image needed
The whole suite finishes in a few seconds

## Why verbatim

`build.rs` reads the two kernel modules and `include!`s them into this crate on
every build
The only transformation is turning inner doc comments `//!` into ordinary
comments `//` because `include!` cannot expand inner attributes into the middle
of a module body

Comments carry no meaning to the compiler so what runs here is byte for byte
what the kernel compiles

There is no second copy of the filesystem in this crate that could quietly
drift out of agreement with `kernel/src`
That is the whole point and it is not decoration
Porting this crate forward from an earlier commit produced four compile errors
before it produced a single test result
`FsNode::File` had changed from `String` to `Vec<u8>` `build_superblock` had
lost an argument and `fs.rs` had gained a `spin::Mutex`
A crate holding its own copy of the format would have kept passing and would
have been testing nothing

## What is replaced

Only what cannot exist on a host

| Kernel service | Stand-in | Why |
|---|---|---|
| `crate::ata` | a sparse in memory disk | there are no PIO ports to talk to |
| `serial_println!` | a sink | there is no UART |
| `x86_64::instructions::interrupts` | `shims/x86_64` | the real one runs `cli` and `sti` which fault outside the kernel |

The interrupt shim narrows what these tests can see and that is worth stating
plainly

`fs.rs` guards every mutation with `without_interrupts`
On the host the closure simply runs and `TEST_LOCK` provides the serialisation
instead
So these tests cover the filesystem logic and not the kernel interrupt
discipline
A bug that only appears when an interrupt lands inside one of those sections
will not be caught here

## The suites

| File | Tests | Covers |
|---|---|---|
| `tests/serialization.rs` | 20 | the record format round trip truncation bad lengths bad UTF-8 unknown record kinds and the legacy v2 superblock |
| `tests/invariants.rs` | 9 | properties that must hold across the whole layer including a fuzz pass over random and mutated blobs |
| `tests/resilience.rs` | 12 | the v3 checkpoint scheme against damaged headers damaged payloads and a disk that fails |

41 tests and all of them pass at `24287c3`

## What `resilience.rs` attacks

The on disk scheme as `write_checkpoint` produces it

```
slot LBA        512 byte header
                  [0..8]   SLOT_MAGIC
                  [8..16]  generation, u64 little endian
                  [16..20] payload length, u32 little endian
                  [20..24] CRC32 of the payload
                  [24..28] SLOT_COMMITTED, present only once committed
slot LBA + 1..  the payload
```

Written header uncommitted then payload then header committed
A mount takes the highest generation among slots that are committed and whose
CRC matches and falls back to the other slot otherwise

Each test damages exactly one property of that scheme

Wrong magic
A length larger than the slot
A length exactly one byte past the slot which an off by one would let through
where an absurd value would not
A payload byte altered inside the recorded length
A header left uncommitted which is the state a power cut leaves behind
Both slots damaged at once
A disk that fails every read
A disk that fails partway through writing the new checkpoint

The assertion is not that mount fails
A fresh filesystem already carries one valid checkpoint so damaging a later one
leaves a good older copy and recovery is entitled to use it
What is asserted is that the damaged bytes never come back as data

## Note on the rig

`with_fresh_fs` leaves the simulated disk completely blank rather than
pre writing a superblock and callers must not wipe it again afterwards

`tuwaiqfs.rs` keeps its format version active slot and generation in statics
that outlive any one test and this crate cannot reach them
A fully zeroed disk is the one input that makes the kernel reset all three
itself because `mount` treats it as unformatted and runs `format_region`

Writing a superblock first looks tidier and is wrong
`mount` then looks for checkpoints finds none and returns before it reaches
those stores
The statics keep the previous test values and the damage surfaces as an
unrelated test failing only when the suite runs as a whole

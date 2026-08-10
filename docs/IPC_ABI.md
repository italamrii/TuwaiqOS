# TuwaiqOS IPC and Capability ABI v1

## Status and byte order

This document defines version 1 of the bounded IPC/capability subsystem. It
does not stabilize unrelated pre-Phase-8 syscalls or complete the broader
native ABI compatibility policy. All fields are little-endian and structures
use the listed fixed offsets. Every request begins with:

| Offset | Type | Field | Required value |
|---:|---|---|---|
| 0 | `u16` | `version` | `1` |
| 2 | `u16` | `size` | exact structure size |
| 4 | `u32` | `flags` | `0` |

Unknown versions, sizes, flags, reserved values, and syscall numbers fail
without publishing a message or capability. Handles and correlation tokens are
opaque. Applications must never interpret, persist, derive, or transfer their
numeric values as authority.

## Syscalls

All operations use `int 0x80`, with the syscall number in `RAX`, structure
pointer in `RDI`, exact structure length in `RSI`, and zero in `RDX`.

| # | Operation | Structure | Behavior |
|---:|---|---|---|
| 22 | `endpoint_create` | `EndpointCreateV1` | create endpoint and return owner handle |
| 23 | `endpoint_close` | `HandleV1` | close endpoint and wake/cancel peers |
| 24 | `send` | `MessageV1` | blocking bounded send |
| 25 | `receive` | `MessageV1` | blocking bounded receive |
| 26 | `try_send` | `MessageV1` | nonblocking send |
| 27 | `try_receive` | `MessageV1` | nonblocking receive |
| 28 | `call` | `MessageV1` | request and wait for one reply |
| 29 | `reply` | `MessageV1` | authorized one-shot reply |
| 30 | `capability_close` | `HandleV1` | close/revoke a capability or revoker |
| 31 | `capability_delegate` | `DelegateV1` | delegate reduced rights; return revoker |
| 32 | `capability_accept` | `AcceptV1` | accept next delegated handle |
| 33 | `fs_scope_create` | `FsScopeV1` | scope caller-private VFS authority |
| 34 | `fs_read` | `FsRequestV1` | bounded whole-file read through scope |
| 35 | `fs_put` | `FsRequestV1` | bounded atomic whole-file replacement |
| 36 | `fs_list` | `FsRequestV1` | bounded newline-delimited directory list |
| 37 | `capability_query` | `CapQueryV1` | inspect local rights and object kind |

The kernel copies and validates the complete fixed structure before object
lookup. Receive/call additionally validate the complete writable structure
before consuming a message or publishing a reply. File operations validate
every nested buffer before VFS work.

## Structures

### `EndpointCreateV1` (16 bytes)

| Offset | Type | Field | Rule |
|---:|---|---|---|
| 0 | header | | v1, size 16, flags 0 |
| 8 | `u32` | `depth` | 1 through 8 |
| 12 | `u32` | `reserved` | 0 |

### `HandleV1` (16 bytes)

| Offset | Type | Field |
|---:|---|---|
| 0 | header | v1, size 16, flags 0 |
| 8 | `u64` | opaque process-local handle |

### `AcceptV1` (16 bytes)

| Offset | Type | Field | Rule |
|---:|---|---|---|
| 0 | header | | v1, size 16, flags 0 |
| 8 | `u64` | `timeout_ticks` | 0 means no deadline; maximum 10,000 |

### `CapQueryV1` (32 bytes)

| Offset | Type | Field | Rule |
|---:|---|---|---|
| 0 | header | | v1, size 32, flags 0 |
| 8 | `u64` | `handle` | process-local capability |
| 16 | `u32` | `rights` | output: effective rights bits |
| 20 | `u32` | `object_kind` | output: `1` endpoint, `2` VFS scope, `3` revoker |
| 24 | `u64` | `reserved` | must be 0 on input; remains 0 |

`capability_query` validates the complete writable structure before lookup and
writes only `rights` and `object_kind`. It never exposes kernel pointers,
slot indexes, or grant identifiers.

### `MessageV1` (304 bytes)

| Offset | Type | Field | Rule |
|---:|---|---|---|
| 0 | header | | v1, size 304, flags 0 |
| 8 | `u64` | `handle` | endpoint capability |
| 16 | `u32` | `message_type` | nonzero when sending/replying |
| 20 | `u32` | `payload_len` | 0 through 256 |
| 24 | `u64` | `timeout_ticks` | blocking deadline; see below |
| 32 | `u64` | `correlation` | zero on send/call input; kernel output on receive |
| 40 | `u32` | `sender_pid` | ignored on input; kernel output on receive/reply |
| 44 | `u32` | `reserved` | 0 |
| 48 | `[u8;256]` | `payload` | only `payload_len` bytes are meaningful |

`try_send` and `try_receive` require timeout zero. Blocking send/receive use
zero for no deadline or 1 through 10,000 ticks. `call` requires 1 through
10,000; `reply` requires zero. The PIT tick rate is 100 Hz.

### `DelegateV1` (160 bytes)

| Offset | Type | Field | Rule |
|---:|---|---|---|
| 0 | header | | v1, size 160, flags 0 |
| 8 | `u64` | `source` | capability holding `DELEGATE` |
| 16 | `u32` | `target_pid` | live, different Ring 3 process |
| 20 | `u32` | `rights` | nonzero subset of source rights |
| 24 | `u16` | `path_len` | 0 through 120 |
| 26 | `u16` | `reserved16` | 0 |
| 28 | `u32` | `reserved32` | 0 |
| 32 | `[u8;128]` | `path` | empty for endpoint; relative narrowed VFS path |

The return value is a close-only revoker for this delegation, not the target's
handle. The target obtains its handle through `capability_accept`.

### `FsScopeV1` (136 bytes)

| Offset | Type | Field | Rule |
|---:|---|---|---|
| 0 | header | | v1, size 136, flags 0 |
| 8 | `u32` | `rights` | file rights plus optional `DELEGATE`/`CLOSE` |
| 12 | `u16` | `path_len` | 1 through 120 |
| 14 | `u16` | `reserved` | 0 |
| 16 | `[u8;120]` | `path` | existing path inside caller's private data root |

### `FsRequestV1` (176 bytes)

| Offset | Type | Field | Rule |
|---:|---|---|---|
| 0 | header | | v1, size 176, flags 0 |
| 8 | `u64` | `handle` | VFS scope capability |
| 16 | `u16` | `path_len` | 0 through 120 |
| 18 | `u16` | `reserved16` | 0 |
| 20 | `u32` | `reserved32` | 0 |
| 24 | `u64` | `data_ptr` | `fs_put` source; otherwise zero |
| 32 | `u64` | `data_len` | `fs_put`: 1 through 4096 |
| 40 | `u64` | `out_ptr` | `fs_read`/`fs_list` destination |
| 48 | `u64` | `out_len` | `fs_read`/`fs_list`: 1 through 4096 |
| 56 | `[u8;120]` | `path` | empty for scope root, otherwise relative child |

## Rights

| Bit | Name | Authority |
|---:|---|---|
| 0 | `SEND` | enqueue endpoint message/call |
| 1 | `RECEIVE` | dequeue endpoint message |
| 2 | `REPLY` | become authorized by receiving a call and reply once |
| 3 | `DELEGATE` | create a reduced child grant |
| 4 | `CLOSE` | close the referenced object |
| 5 | `FILE_READ` | bounded scoped file read |
| 6 | `FILE_WRITE` | bounded scoped whole-file replacement |
| 7 | `FILE_LIST` | bounded scoped directory listing |

Delegation can only remove rights. Handles resolve in the calling process's
table to an object type, object generation, rights, grant generation, and
ownership state. A numeric handle from another process has no meaning.

## Return values

Nonnegative values are success: a new opaque handle, zero, or a byte count.

| Value | Name | Meaning |
|---:|---|---|
| -1 | `INVALID` | malformed pointer, range, size, path, or argument |
| -2 | `VERSION` | unknown ABI version |
| -3 | `FLAGS` | unknown flags |
| -4 | `BAD_HANDLE` | forged, stale, absent, or process-foreign handle |
| -5 | `WRONG_TYPE` | capability object type does not match operation |
| -6 | `RIGHTS` | missing right, amplification, or scope escape |
| -7 | `WOULD_BLOCK` | nonblocking queue/inbox has no progress |
| -8 | `CLOSED` | endpoint closed |
| -9 | `TIMEOUT` | deterministic deadline expired |
| -10 | `PEER_EXITED` | request peer exited while caller waited |
| -11 | `EXHAUSTED` | fixed queue/table/output capacity exhausted |
| -12 | `BAD_MESSAGE` | invalid message fields/reserved values |
| -13 | `REVOKED` | capability grant was revoked |
| -14 | `BAD_TOKEN` | forged, stale, late, or absent correlation token |
| -15 | `DUPLICATE` | reply already completed |

## Lifecycle and lock order

Endpoint close clears queued messages and wakes blocked peers. Process exit
closes owned endpoints/scopes, cancels or fails calls, removes waiters, revokes
descendant grants, releases references, and clears its capability table.
Queued request removal and cleanup use fixed storage and cannot allocate.

The sole IPC state lock disables interrupts and may nest only into the
scheduler for atomic block/wake state changes: `IPC_STATE -> SCHEDULER`. Task
exit/kill never calls IPC cleanup while holding the scheduler lock. User copies,
allocation, VFS/disk work, and unbounded loops do not run while the IPC lock is
held. Receive and call-reply encode into a kernel-owned fixed buffer under the
lock, copy after release, then commit only if the peeked message or reply is
still present.

## VFS scope rules

Initial authority must be inside `/data/<process-name>/`. Delegated paths are
relative to the parent scope, normalized, component-boundary checked, and
required to resolve through the same longest-prefix mount. Absolute children,
`..` escape, prefix confusion, mount crossing, `/apps` replacement, and `/boot`
writes fail. Revocation blocks subsequent operations. One already-authorized,
bounded VFS operation may finish after concurrent revocation; it cannot start a
second operation with the revoked handle.

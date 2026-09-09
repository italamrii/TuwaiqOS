# Tool Registry — Protocol v1

Every tool below is the **only** thing Python is allowed to ask the broker to
do. There is no "run this string as a command" tool, and there never will be
one in this protocol — that is the core security boundary of this system.

Rust is the **only** side that ever calls into the OS. Python only ever
constructs a `ToolRequest` matching one of these exact contracts and sends it
to the broker; it never touches `/proc`, launches processes, or shells out
itself.

---

## Read-only tools

All six below take **no arguments** (`"arguments": {}`) and require no
elevated permission — they only read already-public system state.

### `get_system_info`
Result:
```json
{
  "os_name": "TuwaiqOS",
  "os_version": "v0.5",
  "kernel_version": "string",
  "hostname": "string",
  "uptime_seconds": 12345
}
```

### `get_cpu_info`
Result:
```json
{
  "model": "string",
  "core_count": 8,
  "usage_percent": 23.4,
  "per_core_usage_percent": [12.1, 34.5, ...]
}
```

### `get_memory_info`
Result:
```json
{
  "total_bytes": 17179869184,
  "used_bytes": 14958129152,
  "used_percent": 87.1,
  "top_consumers": [
    {"pid": 4821, "name": "firefox", "bytes": 4508876800}
  ]
}
```

### `get_disk_info`
Result:
```json
{
  "volumes": [
    {"mount_point": "/", "total_bytes": 512110190592,
     "used_bytes": 210110190592, "used_percent": 41.0}
  ]
}
```

### `list_processes`
Result:
```json
{
  "processes": [
    {"pid": 4821, "name": "firefox", "cpu_percent": 12.3, "memory_bytes": 4508876800}
  ]
}
```
Capped server-side at the top 50 by CPU or memory (broker's choice,
documented in `architecture.md`) — never an unbounded dump of every PID.

### `get_network_status`
Result:
```json
{
  "interfaces": [
    {"name": "eth0", "rx_kbps": 12.4, "tx_kbps": 3.1}
  ]
}
```
Excludes loopback. **Does not report link/up-down state** — the
`sysinfo` version this is built on does not expose interface
administrative state on any platform; an earlier attempt inferred it from
MAC address and was demonstrably wrong (misreported known-down virtual
interfaces as up), so the field was removed rather than shipped
unreliable. See `broker/src/procinfo.rs`'s `network_status` docs for the
real platform-specific APIs a future implementation would need.

---

## Action tools (Phase 1)

### `kill_process`

**Sensitive.** Requires explicit user confirmation on a separate turn
before the broker is ever called (see `docs/architecture.md`'s
"Confirmation gate"). The broker additionally enforces its own independent
protection regardless of what Python claims was confirmed.

Arguments:
```json
{ "pid": 4821 }
```

- `pid` must be a real, currently-running process.
- pid `1` is refused unconditionally (it is always the system
  init/supervisor process, on any Unix-like system, regardless of its
  actual name).
- A small set of well-known critical process names (`init`, `systemd`,
  `kernel`, the broker's own name, etc.) is also refused by name — see
  `PROTECTED_PROCESS_NAMES` in `broker/src/tools.rs`.
- Sends `SIGTERM` (graceful termination request), not `SIGKILL`.

Result on success:
```json
{ "pid": 4821, "name": "firefox", "terminated": true }
```

Error codes: `invalid_arguments` (missing/wrong-type `pid`), `not_found`
(no such process), `permission_denied` (protected process).

### `launch_application`

**This is the only tool in Phase 1 that changes system state, and it is
allowlist-only by design — the argument is never a raw command string.**

Arguments:
```json
{ "app_id": "firefox" }
```

- `app_id` **must** be one of the identifiers in the broker's compiled-in
  allowlist (see `ALLOWLIST` in `architecture.md`) — e.g. `firefox`,
  `vscode`, `terminal`, `file_manager`.
- Any `app_id` not in the allowlist → `status: error`, `code:
  not_allowlisted`. The broker does **not** attempt fuzzy matching, path
  lookup, or fall back to executing the string as a command.
- The broker maps `app_id` → a fixed, pre-registered binary path + fixed
  argument list at compile time or from a static config file it owns —
  **never** from anything the request supplies.

Result on success:
```json
{ "app_id": "firefox", "pid": 5190, "launched": true }
```

### `close_application`

**Sensitive.** Requires explicit user confirmation on a separate turn before
the broker is called.

Arguments:
```json
{ "app_id": "firefox" }
```

- `app_id` must be one of the approved application identifiers.
- The broker resolves the allowlisted app to its expected process name and
  refuses unknown apps.

Result on success:
```json
{ "app_id": "firefox", "pid": 4821, "name": "firefox", "terminated": true }
```

### Confirmation gate

`close_application` and `kill_process` share the same workflow:

1. model proposes a structured action request
2. Python stores the exact pending action
3. user must explicitly approve or deny on a later turn
4. only approval triggers the broker call
5. Rust still applies its own allowlist/protected-process checks

---

## Error contract

Every tool, on failure, returns one of the `error.code` values defined in
`schema.json`. Python never receives a raw OS error string, stack trace, or
internal file path — the broker translates internal failures into one of the
fixed codes plus a safe, generic message before returning.

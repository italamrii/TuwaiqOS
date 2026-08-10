//! Versioned, bounded Ring-3 IPC and process-local capabilities.
//!
//! All object, capability, queue, waiter, and call storage is fixed-capacity.
//! The sole state lock is held with interrupts disabled and may nest only into
//! the scheduler for an atomic block/wake transition (`IPC -> scheduler`).
//! User copies and VFS work happen outside this lock. Receive and call-reply
//! paths encode into a kernel-owned fixed buffer under the lock, copy to the
//! user buffer after releasing it, then commit (dequeue / complete) only if
//! the peeked message or call reply is still present.

use alloc::string::String;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use crate::{task, vfs};

pub const ABI_VERSION: u16 = 1;
pub const MAX_MESSAGE_BYTES: usize = 256;
pub const MESSAGE_V1_SIZE: usize = 48 + MAX_MESSAGE_BYTES;
pub const ENDPOINT_CREATE_V1_SIZE: usize = 16;
pub const HANDLE_V1_SIZE: usize = 16;
pub const CAP_QUERY_V1_SIZE: usize = 32;
pub const ACCEPT_V1_SIZE: usize = 16;
pub const DELEGATE_V1_SIZE: usize = 160;
pub const FS_SCOPE_V1_SIZE: usize = 136;
pub const FS_REQUEST_V1_SIZE: usize = 176;

pub const RIGHT_SEND: u32 = 1 << 0;
pub const RIGHT_RECEIVE: u32 = 1 << 1;
pub const RIGHT_REPLY: u32 = 1 << 2;
pub const RIGHT_DELEGATE: u32 = 1 << 3;
pub const RIGHT_CLOSE: u32 = 1 << 4;
pub const RIGHT_FILE_READ: u32 = 1 << 5;
pub const RIGHT_FILE_WRITE: u32 = 1 << 6;
pub const RIGHT_FILE_LIST: u32 = 1 << 7;

const ENDPOINT_RIGHTS: u32 =
    RIGHT_SEND | RIGHT_RECEIVE | RIGHT_REPLY | RIGHT_DELEGATE | RIGHT_CLOSE;
const FILE_RIGHTS: u32 =
    RIGHT_FILE_READ | RIGHT_FILE_WRITE | RIGHT_FILE_LIST | RIGHT_DELEGATE | RIGHT_CLOSE;

const MAX_ENDPOINTS: usize = 64;
const MAX_QUEUE_DEPTH: usize = 8;
const MAX_PROCESSES: usize = 64;
const MAX_CAPS_PER_PROCESS: usize = 32;
const MAX_INBOX: usize = 16;
const MAX_WAITERS: usize = 64;
const MAX_GRANTS: usize = 256;
const MAX_SCOPES: usize = 64;
const MAX_CALLS: usize = 64;
const MAX_GRANT_DEPTH: usize = 16;
const MAX_TIMEOUT_TICKS: u64 = 10_000;
const NO_INDEX: u16 = u16::MAX;

pub const OK: i64 = 0;
pub const ERR_INVALID: i64 = -1;
pub const ERR_VERSION: i64 = -2;
pub const ERR_FLAGS: i64 = -3;
pub const ERR_BAD_HANDLE: i64 = -4;
pub const ERR_WRONG_TYPE: i64 = -5;
pub const ERR_RIGHTS: i64 = -6;
pub const ERR_WOULD_BLOCK: i64 = -7;
pub const ERR_CLOSED: i64 = -8;
pub const ERR_TIMEOUT: i64 = -9;
pub const ERR_PEER_EXITED: i64 = -10;
pub const ERR_EXHAUSTED: i64 = -11;
pub const ERR_BAD_MESSAGE: i64 = -12;
pub const ERR_REVOKED: i64 = -13;
pub const ERR_BAD_TOKEN: i64 = -14;
pub const ERR_DUPLICATE: i64 = -15;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ObjectKind {
    None = 0,
    Endpoint = 1,
    VfsScope = 2,
    Revoker = 3,
}

#[derive(Clone, Copy)]
struct ObjectRef {
    kind: ObjectKind,
    slot: u16,
    generation: u16,
}

impl ObjectRef {
    const NONE: Self = Self {
        kind: ObjectKind::None,
        slot: 0,
        generation: 0,
    };
}

#[derive(Clone, Copy)]
struct CapSlot {
    active: bool,
    revoked: bool,
    generation: u16,
    token: u64,
    object: ObjectRef,
    rights: u32,
    grant_slot: u16,
    grant_generation: u16,
}

impl CapSlot {
    const EMPTY: Self = Self {
        active: false,
        revoked: false,
        generation: 0,
        token: 0,
        object: ObjectRef::NONE,
        rights: 0,
        grant_slot: NO_INDEX,
        grant_generation: 0,
    };
}

#[derive(Clone, Copy)]
struct ProcessCaps {
    active: bool,
    pid: u32,
    caps: [CapSlot; MAX_CAPS_PER_PROCESS],
    inbox: [u64; MAX_INBOX],
    inbox_head: u8,
    inbox_count: u8,
    accept_waiter: u32,
}

impl ProcessCaps {
    const EMPTY: Self = Self {
        active: false,
        pid: 0,
        caps: [CapSlot::EMPTY; MAX_CAPS_PER_PROCESS],
        inbox: [0; MAX_INBOX],
        inbox_head: 0,
        inbox_count: 0,
        accept_waiter: 0,
    };

    fn free_cap(&self) -> Option<usize> {
        self.caps.iter().position(|cap| !cap.active && !cap.revoked)
    }

    fn push_inbox(&mut self, token: u64) -> bool {
        if usize::from(self.inbox_count) >= MAX_INBOX {
            return false;
        }
        let index = (usize::from(self.inbox_head) + usize::from(self.inbox_count)) % MAX_INBOX;
        self.inbox[index] = token;
        self.inbox_count += 1;
        true
    }

    fn pop_inbox(&mut self) -> Option<u64> {
        if self.inbox_count == 0 {
            return None;
        }
        let index = usize::from(self.inbox_head);
        let token = self.inbox[index];
        self.inbox[index] = 0;
        self.inbox_head = ((index + 1) % MAX_INBOX) as u8;
        self.inbox_count -= 1;
        Some(token)
    }
}

#[derive(Clone, Copy)]
struct Grant {
    active: bool,
    generation: u16,
    parent_slot: u16,
    parent_generation: u16,
}

impl Grant {
    const EMPTY: Self = Self {
        active: false,
        generation: 0,
        parent_slot: NO_INDEX,
        parent_generation: 0,
    };
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Message {
    sender_pid: u32,
    message_type: u32,
    payload_len: u16,
    correlation: u64,
    payload: [u8; MAX_MESSAGE_BYTES],
}

impl Message {
    const EMPTY: Self = Self {
        sender_pid: 0,
        message_type: 0,
        payload_len: 0,
        correlation: 0,
        payload: [0; MAX_MESSAGE_BYTES],
    };
}

#[derive(Clone, Copy)]
struct Endpoint {
    active: bool,
    closed: bool,
    generation: u16,
    owner_pid: u32,
    depth: u8,
    head: u8,
    count: u8,
    refs: u16,
    queue: [Message; MAX_QUEUE_DEPTH],
    send_waiters: [u32; MAX_WAITERS],
    send_waiter_count: u8,
    receive_waiters: [u32; MAX_WAITERS],
    receive_waiter_count: u8,
}

impl Endpoint {
    const EMPTY: Self = Self {
        active: false,
        closed: false,
        generation: 0,
        owner_pid: 0,
        depth: 0,
        head: 0,
        count: 0,
        refs: 0,
        queue: [Message::EMPTY; MAX_QUEUE_DEPTH],
        send_waiters: [0; MAX_WAITERS],
        send_waiter_count: 0,
        receive_waiters: [0; MAX_WAITERS],
        receive_waiter_count: 0,
    };

    fn push(&mut self, message: Message) -> bool {
        if self.closed || self.count >= self.depth {
            return false;
        }
        let index = (usize::from(self.head) + usize::from(self.count)) % MAX_QUEUE_DEPTH;
        self.queue[index] = message;
        self.count += 1;
        true
    }

    fn front(&self) -> Option<Message> {
        if self.count == 0 {
            None
        } else {
            Some(self.queue[usize::from(self.head)])
        }
    }

    fn pop(&mut self) -> Option<Message> {
        let message = self.front()?;
        let index = usize::from(self.head);
        self.queue[index] = Message::EMPTY;
        self.head = ((index + 1) % MAX_QUEUE_DEPTH) as u8;
        self.count -= 1;
        Some(message)
    }
}

#[derive(Clone, Copy)]
struct VfsScope {
    active: bool,
    generation: u16,
    owner_pid: u32,
    refs: u16,
    is_directory: bool,
    path_len: u8,
    path: [u8; vfs::PATH_MAX],
}

impl VfsScope {
    const EMPTY: Self = Self {
        active: false,
        generation: 0,
        owner_pid: 0,
        refs: 0,
        is_directory: false,
        path_len: 0,
        path: [0; vfs::PATH_MAX],
    };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallState {
    Waiting,
    Replied,
    Failed,
}

#[derive(Clone, Copy)]
struct CallSlot {
    active: bool,
    generation: u16,
    token: u64,
    endpoint: ObjectRef,
    caller_pid: u32,
    authorized_pid: u32,
    state: CallState,
    error: i64,
    reply: Message,
}

impl CallSlot {
    const EMPTY: Self = Self {
        active: false,
        generation: 0,
        token: 0,
        endpoint: ObjectRef::NONE,
        caller_pid: 0,
        authorized_pid: 0,
        state: CallState::Waiting,
        error: ERR_BAD_TOKEN,
        reply: Message::EMPTY,
    };
}

struct IpcState {
    processes: [ProcessCaps; MAX_PROCESSES],
    grants: [Grant; MAX_GRANTS],
    endpoints: [Endpoint; MAX_ENDPOINTS],
    scopes: [VfsScope; MAX_SCOPES],
    calls: [CallSlot; MAX_CALLS],
}

impl IpcState {
    const fn new() -> Self {
        Self {
            processes: [ProcessCaps::EMPTY; MAX_PROCESSES],
            grants: [Grant::EMPTY; MAX_GRANTS],
            endpoints: [Endpoint::EMPTY; MAX_ENDPOINTS],
            scopes: [VfsScope::EMPTY; MAX_SCOPES],
            calls: [CallSlot::EMPTY; MAX_CALLS],
        }
    }

    fn process_index(&self, pid: u32) -> Option<usize> {
        self.processes
            .iter()
            .position(|process| process.active && process.pid == pid)
    }

    fn ensure_process(&mut self, pid: u32) -> Result<usize, i64> {
        if let Some(index) = self.process_index(pid) {
            return Ok(index);
        }
        let index = self
            .processes
            .iter()
            .position(|process| !process.active)
            .ok_or(ERR_EXHAUSTED)?;
        self.processes[index] = ProcessCaps {
            active: true,
            pid,
            ..ProcessCaps::EMPTY
        };
        Ok(index)
    }

    fn grant_active(&self, slot: u16, generation: u16) -> bool {
        let mut slot = slot;
        let mut generation = generation;
        for _ in 0..MAX_GRANT_DEPTH {
            if slot == NO_INDEX {
                return true;
            }
            let Some(grant) = self.grants.get(usize::from(slot)) else {
                return false;
            };
            if !grant.active || grant.generation != generation {
                return false;
            }
            slot = grant.parent_slot;
            generation = grant.parent_generation;
        }
        slot == NO_INDEX
    }

    fn grant_depth(&self, mut slot: u16, mut generation: u16) -> Option<usize> {
        let mut depth = 0usize;
        while slot != NO_INDEX {
            if depth >= MAX_GRANT_DEPTH {
                return None;
            }
            let grant = self.grants.get(usize::from(slot))?;
            if !grant.active || grant.generation != generation {
                return None;
            }
            depth += 1;
            slot = grant.parent_slot;
            generation = grant.parent_generation;
        }
        Some(depth)
    }

    fn cap(&self, pid: u32, token: u64) -> Result<CapSlot, i64> {
        let process = &self.processes[self.process_index(pid).ok_or(ERR_BAD_HANDLE)?];
        let cap = process
            .caps
            .iter()
            .find(|cap| (cap.active || cap.revoked) && cap.token == token)
            .copied()
            .ok_or(ERR_BAD_HANDLE)?;
        if cap.revoked || !self.grant_active(cap.grant_slot, cap.grant_generation) {
            return Err(ERR_REVOKED);
        }
        Ok(cap)
    }

    fn require_cap(
        &self,
        pid: u32,
        token: u64,
        kind: ObjectKind,
        rights: u32,
    ) -> Result<CapSlot, i64> {
        let cap = self.cap(pid, token)?;
        if cap.object.kind != kind {
            return Err(ERR_WRONG_TYPE);
        }
        if cap.rights & rights != rights {
            return Err(ERR_RIGHTS);
        }
        Ok(cap)
    }

    fn endpoint_index(&self, reference: ObjectRef) -> Result<usize, i64> {
        let index = usize::from(reference.slot);
        let endpoint = self.endpoints.get(index).ok_or(ERR_CLOSED)?;
        if !endpoint.active || endpoint.generation != reference.generation {
            return Err(ERR_CLOSED);
        }
        Ok(index)
    }

    fn scope_index(&self, reference: ObjectRef) -> Result<usize, i64> {
        let index = usize::from(reference.slot);
        let scope = self.scopes.get(index).ok_or(ERR_REVOKED)?;
        if !scope.active || scope.generation != reference.generation {
            return Err(ERR_REVOKED);
        }
        Ok(index)
    }

    fn allocate_grant(&mut self, parent: Option<(u16, u16)>) -> Result<(u16, u16), i64> {
        let index = self
            .grants
            .iter()
            .position(|grant| !grant.active)
            .ok_or(ERR_EXHAUSTED)?;
        let generation = next_generation(self.grants[index].generation);
        self.grants[index] = Grant {
            active: true,
            generation,
            parent_slot: parent.map(|value| value.0).unwrap_or(NO_INDEX),
            parent_generation: parent.map(|value| value.1).unwrap_or(0),
        };
        Ok((index as u16, generation))
    }

    fn install_cap(
        &mut self,
        process_index: usize,
        cap_index: usize,
        object: ObjectRef,
        rights: u32,
        grant: (u16, u16),
    ) -> u64 {
        let token = next_token();
        let generation = next_generation(self.processes[process_index].caps[cap_index].generation);
        self.processes[process_index].caps[cap_index] = CapSlot {
            active: true,
            revoked: false,
            generation,
            token,
            object,
            rights,
            grant_slot: grant.0,
            grant_generation: grant.1,
        };
        self.add_object_ref(object);
        token
    }

    fn retire_cap(&mut self, process_index: usize, cap_index: usize) -> CapSlot {
        let previous = self.processes[process_index].caps[cap_index];
        self.processes[process_index].caps[cap_index] = CapSlot {
            generation: previous.generation,
            ..CapSlot::EMPTY
        };
        previous
    }

    fn add_object_ref(&mut self, object: ObjectRef) {
        match object.kind {
            ObjectKind::Endpoint => {
                if let Some(endpoint) = self.endpoints.get_mut(usize::from(object.slot)) {
                    endpoint.refs = endpoint.refs.saturating_add(1);
                }
            }
            ObjectKind::VfsScope => {
                if let Some(scope) = self.scopes.get_mut(usize::from(object.slot)) {
                    scope.refs = scope.refs.saturating_add(1);
                }
            }
            ObjectKind::None | ObjectKind::Revoker => {}
        }
    }

    fn drop_object_ref(&mut self, object: ObjectRef) {
        match object.kind {
            ObjectKind::Endpoint => {
                let index = usize::from(object.slot);
                if self.endpoints.get(index).is_some_and(|endpoint| {
                    endpoint.active && endpoint.generation == object.generation
                }) {
                    self.endpoints[index].refs = self.endpoints[index].refs.saturating_sub(1);
                    if self.endpoints[index].refs == 0 {
                        self.close_endpoint_index(index, ERR_CLOSED);
                        self.endpoints[index].active = false;
                    }
                }
            }
            ObjectKind::VfsScope => {
                let index = usize::from(object.slot);
                if self
                    .scopes
                    .get(index)
                    .is_some_and(|scope| scope.active && scope.generation == object.generation)
                {
                    self.scopes[index].refs = self.scopes[index].refs.saturating_sub(1);
                    if self.scopes[index].refs == 0 {
                        self.scopes[index].active = false;
                    }
                }
            }
            ObjectKind::None | ObjectKind::Revoker => {}
        }
    }

    fn add_waiter(waiters: &mut [u32; MAX_WAITERS], count: &mut u8, pid: u32) -> bool {
        if waiters[..usize::from(*count)].contains(&pid) {
            return true;
        }
        if usize::from(*count) >= MAX_WAITERS {
            return false;
        }
        waiters[usize::from(*count)] = pid;
        *count += 1;
        true
    }

    fn remove_waiter(waiters: &mut [u32; MAX_WAITERS], count: &mut u8, pid: u32) {
        let len = usize::from(*count);
        if let Some(index) = waiters[..len].iter().position(|value| *value == pid) {
            waiters.copy_within(index + 1..len, index);
            waiters[len - 1] = 0;
            *count -= 1;
        }
    }

    fn wake_first(waiters: &mut [u32; MAX_WAITERS], count: &mut u8) {
        if *count == 0 {
            return;
        }
        let pid = waiters[0];
        Self::remove_waiter(waiters, count, pid);
        task::wake_ipc_task(pid);
    }

    fn close_endpoint_index(&mut self, index: usize, error: i64) {
        if !self.endpoints[index].active || self.endpoints[index].closed {
            return;
        }
        self.endpoints[index].closed = true;
        self.endpoints[index].queue = [Message::EMPTY; MAX_QUEUE_DEPTH];
        self.endpoints[index].count = 0;
        self.endpoints[index].head = 0;
        let send_count = self.endpoints[index].send_waiter_count;
        let receive_count = self.endpoints[index].receive_waiter_count;
        for pid in self.endpoints[index].send_waiters[..usize::from(send_count)]
            .iter()
            .copied()
        {
            task::wake_ipc_task(pid);
        }
        for pid in self.endpoints[index].receive_waiters[..usize::from(receive_count)]
            .iter()
            .copied()
        {
            task::wake_ipc_task(pid);
        }
        self.endpoints[index].send_waiter_count = 0;
        self.endpoints[index].receive_waiter_count = 0;
        let reference = ObjectRef {
            kind: ObjectKind::Endpoint,
            slot: index as u16,
            generation: self.endpoints[index].generation,
        };
        for call in self.calls.iter_mut() {
            if call.active
                && same_object(call.endpoint, reference)
                && call.state == CallState::Waiting
            {
                call.state = CallState::Failed;
                call.error = error;
                task::wake_ipc_task(call.caller_pid);
            }
        }
    }

    fn remove_queued_correlation(&mut self, endpoint_ref: ObjectRef, correlation: u64) {
        let Ok(index) = self.endpoint_index(endpoint_ref) else {
            return;
        };
        let endpoint = &mut self.endpoints[index];
        let mut kept = [Message::EMPTY; MAX_QUEUE_DEPTH];
        let mut kept_count = 0usize;
        while let Some(message) = endpoint.pop() {
            if message.correlation != correlation {
                kept[kept_count] = message;
                kept_count += 1;
            }
        }
        endpoint.queue = kept;
        endpoint.head = 0;
        endpoint.count = kept_count as u8;
        if kept_count < usize::from(endpoint.depth) {
            Self::wake_first(&mut endpoint.send_waiters, &mut endpoint.send_waiter_count);
        }
    }

    fn revoke_grant_tree(&mut self, root_slot: u16, root_generation: u16) {
        if root_slot == NO_INDEX {
            return;
        }
        if let Some(root) = self.grants.get_mut(usize::from(root_slot)) {
            if root.active && root.generation == root_generation {
                root.active = false;
            }
        }
        for _ in 0..MAX_GRANT_DEPTH {
            let mut changed = false;
            for index in 0..MAX_GRANTS {
                let grant = self.grants[index];
                if !grant.active || grant.parent_slot == NO_INDEX {
                    continue;
                }
                let parent_active =
                    self.grants
                        .get(usize::from(grant.parent_slot))
                        .is_some_and(|parent| {
                            parent.active && parent.generation == grant.parent_generation
                        });
                if !parent_active {
                    self.grants[index].active = false;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for process_index in 0..MAX_PROCESSES {
            for cap_index in 0..MAX_CAPS_PER_PROCESS {
                let cap = self.processes[process_index].caps[cap_index];
                if cap.active
                    && cap.object.kind != ObjectKind::Revoker
                    && !self.grant_active(cap.grant_slot, cap.grant_generation)
                {
                    self.processes[process_index].caps[cap_index].active = false;
                    self.processes[process_index].caps[cap_index].revoked = true;
                    self.drop_object_ref(cap.object);
                }
            }
        }
    }
}

static STATE: Mutex<IpcState> = Mutex::new(IpcState::new());
static TOKEN_STATE: AtomicU64 = AtomicU64::new(0x54_55_57_41_49_51_08);

fn with_state<R>(f: impl FnOnce(&mut IpcState) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut state = STATE.lock();
        f(&mut state)
    })
}

fn next_generation(previous: u16) -> u16 {
    previous.wrapping_add(1).max(1)
}

fn next_token() -> u64 {
    let sequence = TOKEN_STATE.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    let ticks = crate::interrupts::ticks().rotate_left(17);
    let mut value = sequence ^ ticks ^ unsafe { core::arch::x86_64::_rdtsc() };
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;
    (value & i64::MAX as u64).max(1)
}

fn same_object(left: ObjectRef, right: ObjectRef) -> bool {
    left.kind == right.kind && left.slot == right.slot && left.generation == right.generation
}

fn current_pid() -> Result<u32, i64> {
    task::current_task_id().ok_or(ERR_INVALID)
}

fn read_user_fixed<const N: usize>(ptr: u64, len: u64) -> Result<[u8; N], i64> {
    if len != N as u64 {
        return Err(ERR_INVALID);
    }
    let bytes = task::copy_from_current_user(ptr, N).ok_or(ERR_INVALID)?;
    bytes.try_into().map_err(|_| ERR_INVALID)
}

fn header(bytes: &[u8], expected_size: usize) -> Result<(), i64> {
    if u16::from_le_bytes([bytes[0], bytes[1]]) != ABI_VERSION {
        return Err(ERR_VERSION);
    }
    if usize::from(u16::from_le_bytes([bytes[2], bytes[3]])) != expected_size {
        return Err(ERR_INVALID);
    }
    if u32::from_le_bytes(bytes[4..8].try_into().map_err(|_| ERR_INVALID)?) != 0 {
        return Err(ERR_FLAGS);
    }
    Ok(())
}

fn le_u64(bytes: &[u8], offset: usize) -> Result<u64, i64> {
    Ok(u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .map_err(|_| ERR_INVALID)?,
    ))
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32, i64> {
    Ok(u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .map_err(|_| ERR_INVALID)?,
    ))
}

fn le_u16(bytes: &[u8], offset: usize) -> Result<u16, i64> {
    Ok(u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .map_err(|_| ERR_INVALID)?,
    ))
}

#[derive(Clone, Copy)]
struct UserMessage {
    handle: u64,
    message_type: u32,
    payload_len: usize,
    timeout_ticks: u64,
    correlation: u64,
    payload: [u8; MAX_MESSAGE_BYTES],
}

fn parse_message(ptr: u64, len: u64) -> Result<UserMessage, i64> {
    let bytes = read_user_fixed::<MESSAGE_V1_SIZE>(ptr, len)?;
    header(&bytes, MESSAGE_V1_SIZE)?;
    let payload_len = usize::try_from(le_u32(&bytes, 20)?).map_err(|_| ERR_BAD_MESSAGE)?;
    if payload_len > MAX_MESSAGE_BYTES || le_u32(&bytes, 44)? != 0 {
        return Err(ERR_BAD_MESSAGE);
    }
    let mut payload = [0u8; MAX_MESSAGE_BYTES];
    payload[..payload_len].copy_from_slice(&bytes[48..48 + payload_len]);
    Ok(UserMessage {
        handle: le_u64(&bytes, 8)?,
        message_type: le_u32(&bytes, 16)?,
        payload_len,
        timeout_ticks: le_u64(&bytes, 24)?,
        correlation: le_u64(&bytes, 32)?,
        payload,
    })
}

fn encode_message(handle: u64, message: Message) -> [u8; MESSAGE_V1_SIZE] {
    let mut bytes = [0u8; MESSAGE_V1_SIZE];
    bytes[0..2].copy_from_slice(&ABI_VERSION.to_le_bytes());
    bytes[2..4].copy_from_slice(&(MESSAGE_V1_SIZE as u16).to_le_bytes());
    bytes[8..16].copy_from_slice(&handle.to_le_bytes());
    bytes[16..20].copy_from_slice(&message.message_type.to_le_bytes());
    bytes[20..24].copy_from_slice(&u32::from(message.payload_len).to_le_bytes());
    bytes[32..40].copy_from_slice(&message.correlation.to_le_bytes());
    bytes[40..44].copy_from_slice(&message.sender_pid.to_le_bytes());
    bytes[48..48 + usize::from(message.payload_len)]
        .copy_from_slice(&message.payload[..usize::from(message.payload_len)]);
    bytes
}

fn deadline(timeout_ticks: u64, blocking: bool) -> Result<u64, i64> {
    if !blocking {
        if timeout_ticks != 0 {
            return Err(ERR_BAD_MESSAGE);
        }
        return Ok(0);
    }
    if timeout_ticks > MAX_TIMEOUT_TICKS {
        return Err(ERR_BAD_MESSAGE);
    }
    if timeout_ticks == 0 {
        Ok(u64::MAX)
    } else {
        crate::interrupts::ticks()
            .checked_add(timeout_ticks)
            .ok_or(ERR_BAD_MESSAGE)
    }
}

fn expired(deadline: u64) -> bool {
    deadline != u64::MAX && crate::interrupts::ticks() >= deadline
}

pub fn sys_endpoint_create(ptr: u64, len: u64) -> i64 {
    let bytes = match read_user_fixed::<ENDPOINT_CREATE_V1_SIZE>(ptr, len) {
        Ok(bytes) => bytes,
        Err(error) => return error,
    };
    if let Err(error) = header(&bytes, ENDPOINT_CREATE_V1_SIZE) {
        return error;
    }
    let depth = match le_u32(&bytes, 8) {
        Ok(value) if value != 0 && value as usize <= MAX_QUEUE_DEPTH => value as u8,
        _ => return ERR_INVALID,
    };
    if le_u32(&bytes, 12).unwrap_or(1) != 0 {
        return ERR_BAD_MESSAGE;
    }
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    with_state(|state| {
        let process_index = state.ensure_process(pid)?;
        let cap_index = state.processes[process_index]
            .free_cap()
            .ok_or(ERR_EXHAUSTED)?;
        let endpoint_index = state
            .endpoints
            .iter()
            .position(|endpoint| !endpoint.active)
            .ok_or(ERR_EXHAUSTED)?;
        let grant = state.allocate_grant(None)?;
        let endpoint_generation = next_generation(state.endpoints[endpoint_index].generation);
        state.endpoints[endpoint_index] = Endpoint {
            active: true,
            closed: false,
            generation: endpoint_generation,
            owner_pid: pid,
            depth,
            ..Endpoint::EMPTY
        };
        let object = ObjectRef {
            kind: ObjectKind::Endpoint,
            slot: endpoint_index as u16,
            generation: endpoint_generation,
        };
        let token = state.install_cap(process_index, cap_index, object, ENDPOINT_RIGHTS, grant);
        Ok::<i64, i64>(token as i64)
    })
    .unwrap_or_else(|error| error)
}

fn parse_handle(ptr: u64, len: u64) -> Result<u64, i64> {
    let bytes = read_user_fixed::<HANDLE_V1_SIZE>(ptr, len)?;
    header(&bytes, HANDLE_V1_SIZE)?;
    Ok(le_u64(&bytes, 8)?)
}

pub fn sys_endpoint_close(ptr: u64, len: u64) -> i64 {
    let handle = match parse_handle(ptr, len) {
        Ok(handle) => handle,
        Err(error) => return error,
    };
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    with_state(|state| {
        let cap = state.require_cap(pid, handle, ObjectKind::Endpoint, RIGHT_CLOSE)?;
        let index = state.endpoint_index(cap.object)?;
        if state.endpoints[index].closed {
            return Err(ERR_CLOSED);
        }
        state.close_endpoint_index(index, ERR_CLOSED);
        Ok(OK)
    })
    .unwrap_or_else(|error| error)
}

fn send_attempt(pid: u32, user: UserMessage, blocking: bool, end: u64) -> Result<bool, i64> {
    with_state(|state| {
        let cap = state.require_cap(pid, user.handle, ObjectKind::Endpoint, RIGHT_SEND)?;
        let index = state.endpoint_index(cap.object)?;
        if state.endpoints[index].closed {
            return Err(ERR_CLOSED);
        }
        if state.endpoints[index].count >= state.endpoints[index].depth {
            if !blocking {
                return Err(ERR_WOULD_BLOCK);
            }
            if expired(end) {
                IpcState::remove_waiter(
                    &mut state.endpoints[index].send_waiters,
                    &mut state.endpoints[index].send_waiter_count,
                    pid,
                );
                return Err(ERR_TIMEOUT);
            }
            if !IpcState::add_waiter(
                &mut state.endpoints[index].send_waiters,
                &mut state.endpoints[index].send_waiter_count,
                pid,
            ) {
                return Err(ERR_EXHAUSTED);
            }
            task::block_current_for_ipc(end);
            return Ok(false);
        }
        IpcState::remove_waiter(
            &mut state.endpoints[index].send_waiters,
            &mut state.endpoints[index].send_waiter_count,
            pid,
        );
        let message = Message {
            sender_pid: pid,
            message_type: user.message_type,
            payload_len: user.payload_len as u16,
            correlation: 0,
            payload: user.payload,
        };
        if !state.endpoints[index].push(message) {
            return Err(ERR_CLOSED);
        }
        IpcState::wake_first(
            &mut state.endpoints[index].receive_waiters,
            &mut state.endpoints[index].receive_waiter_count,
        );
        Ok(true)
    })
}

fn sys_send_common(ptr: u64, len: u64, blocking: bool) -> i64 {
    let user = match parse_message(ptr, len) {
        Ok(user) => user,
        Err(error) => return error,
    };
    if user.message_type == 0 || user.correlation != 0 {
        return ERR_BAD_MESSAGE;
    }
    let end = match deadline(user.timeout_ticks, blocking) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    loop {
        match send_attempt(pid, user, blocking, end) {
            Ok(true) => return OK,
            Ok(false) => task::schedule(),
            Err(error) => return error,
        }
    }
}

pub fn sys_send(ptr: u64, len: u64) -> i64 {
    sys_send_common(ptr, len, true)
}

pub fn sys_try_send(ptr: u64, len: u64) -> i64 {
    sys_send_common(ptr, len, false)
}

fn receive_attempt(
    pid: u32,
    user_ptr: u64,
    user: UserMessage,
    blocking: bool,
    end: u64,
) -> Result<Option<i64>, i64> {
    // Peek and encode under the lock; perform the user copy with interrupts
    // enabled; commit the dequeue only if the same message remains at the front.
    let peeked = with_state(
        |state| -> Result<Option<(ObjectRef, Message, [u8; MESSAGE_V1_SIZE], Option<usize>)>, i64> {
            let cap = state.require_cap(pid, user.handle, ObjectKind::Endpoint, RIGHT_RECEIVE)?;
            let index = state.endpoint_index(cap.object)?;
            if let Some(message) = state.endpoints[index].front() {
                let authorized_call = if message.correlation != 0 {
                    if cap.rights & RIGHT_REPLY == 0 {
                        return Err(ERR_RIGHTS);
                    }
                    let Some(call_index) = state
                        .calls
                        .iter()
                        .position(|call| call.active && call.token == message.correlation)
                    else {
                        return Err(ERR_BAD_TOKEN);
                    };
                    let call = state.calls[call_index];
                    if call.state != CallState::Waiting || !same_object(call.endpoint, cap.object) {
                        return Err(ERR_BAD_TOKEN);
                    }
                    Some(call_index)
                } else {
                    None
                };
                let encoded = encode_message(user.handle, message);
                return Ok(Some((cap.object, message, encoded, authorized_call)));
            }
            if state.endpoints[index].closed {
                return Err(ERR_CLOSED);
            }
            if !blocking {
                return Err(ERR_WOULD_BLOCK);
            }
            if expired(end) {
                IpcState::remove_waiter(
                    &mut state.endpoints[index].receive_waiters,
                    &mut state.endpoints[index].receive_waiter_count,
                    pid,
                );
                return Err(ERR_TIMEOUT);
            }
            if !IpcState::add_waiter(
                &mut state.endpoints[index].receive_waiters,
                &mut state.endpoints[index].receive_waiter_count,
                pid,
            ) {
                return Err(ERR_EXHAUSTED);
            }
            task::block_current_for_ipc(end);
            Ok(None)
        },
    )?;

    let Some((object, message, encoded, authorized_call)) = peeked else {
        return Ok(None);
    };
    if !task::copy_to_current_user(user_ptr, &encoded) {
        return Err(ERR_INVALID);
    }
    with_state(|state| {
        let index = state.endpoint_index(object)?;
        let Some(front) = state.endpoints[index].front() else {
            return if blocking {
                Ok(None)
            } else {
                Err(ERR_WOULD_BLOCK)
            };
        };
        if front != message {
            return if blocking {
                Ok(None)
            } else {
                Err(ERR_WOULD_BLOCK)
            };
        }
        if let Some(call_index) = authorized_call {
            let call = state.calls[call_index];
            if !(call.active
                && call.token == message.correlation
                && call.state == CallState::Waiting
                && same_object(call.endpoint, object))
            {
                return Err(ERR_BAD_TOKEN);
            }
            state.calls[call_index].authorized_pid = pid;
        }
        state.endpoints[index].pop();
        IpcState::remove_waiter(
            &mut state.endpoints[index].receive_waiters,
            &mut state.endpoints[index].receive_waiter_count,
            pid,
        );
        IpcState::wake_first(
            &mut state.endpoints[index].send_waiters,
            &mut state.endpoints[index].send_waiter_count,
        );
        Ok(Some(i64::from(message.payload_len)))
    })
}

fn sys_receive_common(ptr: u64, len: u64, blocking: bool) -> i64 {
    let user = match parse_message(ptr, len) {
        Ok(user) => user,
        Err(error) => return error,
    };
    if user.payload_len != 0 || user.correlation != 0 {
        return ERR_BAD_MESSAGE;
    }
    if !task::validate_current_user_range(ptr, MESSAGE_V1_SIZE, true) {
        return ERR_INVALID;
    }
    let end = match deadline(user.timeout_ticks, blocking) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    loop {
        match receive_attempt(pid, ptr, user, blocking, end) {
            Ok(Some(result)) => return result,
            Ok(None) => task::schedule(),
            Err(error) => return error,
        }
    }
}

pub fn sys_receive(ptr: u64, len: u64) -> i64 {
    sys_receive_common(ptr, len, true)
}

pub fn sys_try_receive(ptr: u64, len: u64) -> i64 {
    sys_receive_common(ptr, len, false)
}

fn begin_call(pid: u32, user: UserMessage, end: u64) -> Result<Option<u64>, i64> {
    with_state(|state| {
        let cap = state.require_cap(pid, user.handle, ObjectKind::Endpoint, RIGHT_SEND)?;
        let endpoint_index = state.endpoint_index(cap.object)?;
        if state.endpoints[endpoint_index].closed {
            return Err(ERR_CLOSED);
        }
        if state.endpoints[endpoint_index].count >= state.endpoints[endpoint_index].depth {
            if expired(end) {
                IpcState::remove_waiter(
                    &mut state.endpoints[endpoint_index].send_waiters,
                    &mut state.endpoints[endpoint_index].send_waiter_count,
                    pid,
                );
                return Err(ERR_TIMEOUT);
            }
            if !IpcState::add_waiter(
                &mut state.endpoints[endpoint_index].send_waiters,
                &mut state.endpoints[endpoint_index].send_waiter_count,
                pid,
            ) {
                return Err(ERR_EXHAUSTED);
            }
            task::block_current_for_ipc(end);
            return Ok(None);
        }
        let call_index = state
            .calls
            .iter()
            .position(|call| !call.active)
            .ok_or(ERR_EXHAUSTED)?;
        let token = next_token();
        let generation = next_generation(state.calls[call_index].generation);
        state.calls[call_index] = CallSlot {
            active: true,
            generation,
            token,
            endpoint: cap.object,
            caller_pid: pid,
            authorized_pid: 0,
            state: CallState::Waiting,
            error: 0,
            reply: Message::EMPTY,
        };
        let message = Message {
            sender_pid: pid,
            message_type: user.message_type,
            payload_len: user.payload_len as u16,
            correlation: token,
            payload: user.payload,
        };
        if !state.endpoints[endpoint_index].push(message) {
            state.calls[call_index].active = false;
            return Err(ERR_CLOSED);
        }
        IpcState::remove_waiter(
            &mut state.endpoints[endpoint_index].send_waiters,
            &mut state.endpoints[endpoint_index].send_waiter_count,
            pid,
        );
        IpcState::wake_first(
            &mut state.endpoints[endpoint_index].receive_waiters,
            &mut state.endpoints[endpoint_index].receive_waiter_count,
        );
        task::block_current_for_ipc(end);
        Ok(Some(token))
    })
}

fn finish_call(pid: u32, token: u64, user_ptr: u64, handle: u64, end: u64) -> i64 {
    enum Finish {
        Replied {
            encoded: [u8; MESSAGE_V1_SIZE],
            payload_len: u16,
            generation: u16,
        },
        Failed(i64),
        Wait,
    }

    let outcome = with_state(|state| {
        let Some(index) = state
            .calls
            .iter()
            .position(|call| call.active && call.token == token && call.caller_pid == pid)
        else {
            return Err(ERR_BAD_TOKEN);
        };
        match state.calls[index].state {
            CallState::Replied => {
                let encoded = encode_message(handle, state.calls[index].reply);
                Ok(Finish::Replied {
                    encoded,
                    payload_len: state.calls[index].reply.payload_len,
                    generation: state.calls[index].generation,
                })
            }
            CallState::Failed => {
                let error = state.calls[index].error;
                state.calls[index].active = false;
                Ok(Finish::Failed(error))
            }
            CallState::Waiting => {
                if expired(end) {
                    state.calls[index].active = false;
                    state.remove_queued_correlation(state.calls[index].endpoint, token);
                    Ok(Finish::Failed(ERR_TIMEOUT))
                } else {
                    task::block_current_for_ipc(end);
                    Ok(Finish::Wait)
                }
            }
        }
    });

    match outcome {
        Err(error) => error,
        Ok(Finish::Failed(error)) => error,
        Ok(Finish::Wait) => i64::MIN,
        Ok(Finish::Replied {
            encoded,
            payload_len,
            generation,
        }) => {
            if !task::copy_to_current_user(user_ptr, &encoded) {
                return ERR_INVALID;
            }
            with_state(|state| {
                let Some(index) = state.calls.iter().position(|call| {
                    call.active
                        && call.token == token
                        && call.caller_pid == pid
                        && call.generation == generation
                        && call.state == CallState::Replied
                }) else {
                    return ERR_BAD_TOKEN;
                };
                state.calls[index].active = false;
                i64::from(payload_len)
            })
        }
    }
}

pub fn sys_call(ptr: u64, len: u64) -> i64 {
    let user = match parse_message(ptr, len) {
        Ok(user) => user,
        Err(error) => return error,
    };
    if user.message_type == 0 || user.correlation != 0 || user.timeout_ticks == 0 {
        return ERR_BAD_MESSAGE;
    }
    if !task::validate_current_user_range(ptr, MESSAGE_V1_SIZE, true) {
        return ERR_INVALID;
    }
    let end = match deadline(user.timeout_ticks, true) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    let token = loop {
        match begin_call(pid, user, end) {
            Ok(Some(token)) => break token,
            Ok(None) => task::schedule(),
            Err(error) => return error,
        }
    };
    task::schedule();
    loop {
        let result = finish_call(pid, token, ptr, user.handle, end);
        if result != i64::MIN {
            return result;
        }
        task::schedule();
    }
}

pub fn sys_reply(ptr: u64, len: u64) -> i64 {
    let user = match parse_message(ptr, len) {
        Ok(user) => user,
        Err(error) => return error,
    };
    if user.message_type == 0 || user.correlation == 0 || user.timeout_ticks != 0 {
        return ERR_BAD_MESSAGE;
    }
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    with_state(|state| {
        let cap = state.require_cap(pid, user.handle, ObjectKind::Endpoint, RIGHT_REPLY)?;
        let Some(index) = state
            .calls
            .iter()
            .position(|call| call.active && call.token == user.correlation)
        else {
            return Err(ERR_BAD_TOKEN);
        };
        if !same_object(state.calls[index].endpoint, cap.object)
            || state.calls[index].authorized_pid != pid
        {
            return Err(ERR_RIGHTS);
        }
        if state.calls[index].state != CallState::Waiting {
            return Err(ERR_DUPLICATE);
        }
        state.calls[index].reply = Message {
            sender_pid: pid,
            message_type: user.message_type,
            payload_len: user.payload_len as u16,
            correlation: user.correlation,
            payload: user.payload,
        };
        state.calls[index].state = CallState::Replied;
        task::wake_ipc_task(state.calls[index].caller_pid);
        Ok(OK)
    })
    .unwrap_or_else(|error| error)
}

pub fn sys_capability_close(ptr: u64, len: u64) -> i64 {
    let token = match parse_handle(ptr, len) {
        Ok(token) => token,
        Err(error) => return error,
    };
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    with_state(|state| {
        let process_index = state.process_index(pid).ok_or(ERR_BAD_HANDLE)?;
        let cap_index = state.processes[process_index]
            .caps
            .iter()
            .position(|cap| (cap.active || cap.revoked) && cap.token == token)
            .ok_or(ERR_BAD_HANDLE)?;
        let cap = state.processes[process_index].caps[cap_index];
        if cap.revoked {
            state.retire_cap(process_index, cap_index);
            return Err(ERR_REVOKED);
        }
        state.retire_cap(process_index, cap_index);
        if cap.object.kind == ObjectKind::Revoker {
            state.revoke_grant_tree(cap.object.slot, cap.object.generation);
        } else {
            state.drop_object_ref(cap.object);
            state.revoke_grant_tree(cap.grant_slot, cap.grant_generation);
        }
        Ok(OK)
    })
    .unwrap_or_else(|error| error)
}

/// Inspect a process-local capability without granting new authority.
///
/// Writes `rights` and `object_kind` into the caller's CapQueryV1 buffer after
/// validating the complete writable structure. No kernel pointer or grant
/// index is exposed.
pub fn sys_capability_query(ptr: u64, len: u64) -> i64 {
    if len != CAP_QUERY_V1_SIZE as u64 {
        return ERR_INVALID;
    }
    if !task::validate_current_user_range(ptr, CAP_QUERY_V1_SIZE, true) {
        return ERR_INVALID;
    }
    let bytes = match read_user_fixed::<CAP_QUERY_V1_SIZE>(ptr, len) {
        Ok(bytes) => bytes,
        Err(error) => return error,
    };
    if let Err(error) = header(&bytes, CAP_QUERY_V1_SIZE) {
        return error;
    }
    let reserved = match le_u64(&bytes, 24) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if reserved != 0 {
        return ERR_BAD_MESSAGE;
    }
    let token = match le_u64(&bytes, 8) {
        Ok(token) => token,
        Err(error) => return error,
    };
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    let (rights, kind) = match with_state(|state| -> Result<(u32, u32), i64> {
        let process_index = state.process_index(pid).ok_or(ERR_BAD_HANDLE)?;
        let cap = state.processes[process_index]
            .caps
            .iter()
            .find(|cap| (cap.active || cap.revoked) && cap.token == token)
            .copied()
            .ok_or(ERR_BAD_HANDLE)?;
        if cap.revoked {
            return Err(ERR_REVOKED);
        }
        if !cap.active {
            return Err(ERR_BAD_HANDLE);
        }
        Ok((cap.rights, u32::from(cap.object.kind as u8)))
    }) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let mut out = [0u8; CAP_QUERY_V1_SIZE];
    out[..CAP_QUERY_V1_SIZE].copy_from_slice(&bytes);
    out[16..20].copy_from_slice(&rights.to_le_bytes());
    out[20..24].copy_from_slice(&kind.to_le_bytes());
    if !task::copy_to_current_user(ptr, &out) {
        return ERR_INVALID;
    }
    OK
}

#[derive(Clone)]
struct ScopeView {
    object: ObjectRef,
    owner_pid: u32,
    root: String,
    is_directory: bool,
    rights: u32,
    grant: (u16, u16),
}

fn scope_view(pid: u32, token: u64, rights: u32) -> Result<ScopeView, i64> {
    let snapshot = with_state(|state| {
        let cap = state.require_cap(pid, token, ObjectKind::VfsScope, rights)?;
        let index = state.scope_index(cap.object)?;
        let scope = state.scopes[index];
        Ok::<_, i64>((cap, scope))
    })?;
    let text = core::str::from_utf8(&snapshot.1.path[..usize::from(snapshot.1.path_len)])
        .map_err(|_| ERR_REVOKED)?;
    let mut root = String::new();
    root.try_reserve_exact(text.len())
        .map_err(|_| ERR_EXHAUSTED)?;
    root.push_str(text);
    Ok(ScopeView {
        object: snapshot.0.object,
        owner_pid: snapshot.1.owner_pid,
        root,
        is_directory: snapshot.1.is_directory,
        rights: snapshot.0.rights,
        grant: (snapshot.0.grant_slot, snapshot.0.grant_generation),
    })
}

fn parse_delegate(ptr: u64, len: u64) -> Result<(u64, u32, u32, String), i64> {
    let bytes = read_user_fixed::<DELEGATE_V1_SIZE>(ptr, len)?;
    header(&bytes, DELEGATE_V1_SIZE)?;
    if le_u32(&bytes, 28)? != 0 {
        return Err(ERR_BAD_MESSAGE);
    }
    let path_len = usize::from(le_u16(&bytes, 24)?);
    if path_len > vfs::PATH_MAX || le_u16(&bytes, 26)? != 0 {
        return Err(ERR_BAD_MESSAGE);
    }
    let path = core::str::from_utf8(&bytes[32..32 + path_len]).map_err(|_| ERR_INVALID)?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(path_len)
        .map_err(|_| ERR_EXHAUSTED)?;
    owned.push_str(path);
    Ok((
        le_u64(&bytes, 8)?,
        le_u32(&bytes, 16)?,
        le_u32(&bytes, 20)?,
        owned,
    ))
}

fn path_is_within(root: &str, target: &str, root_is_directory: bool) -> bool {
    target == root
        || root_is_directory
            && target
                .strip_prefix(root)
                .is_some_and(|suffix| suffix.starts_with('/'))
}

fn resolve_scope_path(view: &ScopeView, relative: &str) -> Result<String, i64> {
    if relative.is_empty() {
        return Ok(view.root.clone());
    }
    if relative.starts_with('/') {
        return Err(ERR_RIGHTS);
    }
    let target = vfs::normalize(&view.root, relative).map_err(|_| ERR_INVALID)?;
    if !path_is_within(&view.root, &target, view.is_directory)
        || !vfs::same_mount(&view.root, &target).map_err(|_| ERR_RIGHTS)?
    {
        return Err(ERR_RIGHTS);
    }
    Ok(target)
}

pub fn sys_capability_delegate(ptr: u64, len: u64) -> i64 {
    let (source_token, target_pid, requested_rights, path) = match parse_delegate(ptr, len) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    if !task::is_live_user_process(target_pid) {
        return ERR_INVALID;
    }
    if target_pid == pid {
        return ERR_INVALID;
    }
    let (source, narrowed_scope) = match with_state(|state| {
        state.require_cap(pid, source_token, ObjectKind::Endpoint, RIGHT_DELEGATE)
    }) {
        Ok(cap) => (cap, None),
        Err(ERR_WRONG_TYPE) => match scope_view(pid, source_token, RIGHT_DELEGATE) {
            Ok(view) => {
                let target = match resolve_scope_path(&view, &path) {
                    Ok(target) => target,
                    Err(error) => return error,
                };
                let kind = match vfs::kind("/", &target) {
                    Ok(kind) => kind,
                    Err(_) => return ERR_INVALID,
                };
                (
                    CapSlot {
                        active: true,
                        revoked: false,
                        generation: 0,
                        token: source_token,
                        object: view.object,
                        rights: view.rights,
                        grant_slot: view.grant.0,
                        grant_generation: view.grant.1,
                    },
                    Some((target, kind == vfs::NodeKind::Directory, view.owner_pid)),
                )
            }
            Err(error) => return error,
        },
        Err(error) => return error,
    };
    let allowed = match source.object.kind {
        ObjectKind::Endpoint => ENDPOINT_RIGHTS,
        ObjectKind::VfsScope => FILE_RIGHTS,
        _ => return ERR_WRONG_TYPE,
    };
    if requested_rights == 0
        || requested_rights & !allowed != 0
        || source.rights & requested_rights != requested_rights
    {
        return ERR_RIGHTS;
    }
    if source.object.kind == ObjectKind::Endpoint && !path.is_empty() {
        return ERR_BAD_MESSAGE;
    }
    with_state(|state| {
        // VFS path inspection above may temporarily enable interrupts. Check
        // liveness again while IPC state holds the permitted IPC->scheduler
        // lock order, preventing publication to a pid that exited meanwhile.
        if !task::is_live_user_process(target_pid) {
            return Err(ERR_PEER_EXITED);
        }
        let source = state.cap(pid, source_token)?;
        if source.rights & RIGHT_DELEGATE == 0
            || source.rights & requested_rights != requested_rights
        {
            return Err(ERR_RIGHTS);
        }
        if state
            .grant_depth(source.grant_slot, source.grant_generation)
            .ok_or(ERR_REVOKED)?
            >= MAX_GRANT_DEPTH
        {
            return Err(ERR_EXHAUSTED);
        }
        let source_process = state.process_index(pid).ok_or(ERR_BAD_HANDLE)?;
        let target_process = state.ensure_process(target_pid)?;
        let revoker_index = state.processes[source_process]
            .free_cap()
            .ok_or(ERR_EXHAUSTED)?;
        let target_cap_index = state.processes[target_process]
            .free_cap()
            .ok_or(ERR_EXHAUSTED)?;
        if usize::from(state.processes[target_process].inbox_count) >= MAX_INBOX {
            return Err(ERR_EXHAUSTED);
        }
        let grant_free = state
            .grants
            .iter()
            .position(|grant| !grant.active)
            .ok_or(ERR_EXHAUSTED)?;
        let scope_free = if narrowed_scope.is_some() {
            Some(
                state
                    .scopes
                    .iter()
                    .position(|scope| !scope.active)
                    .ok_or(ERR_EXHAUSTED)?,
            )
        } else {
            None
        };
        let grant_generation = next_generation(state.grants[grant_free].generation);
        state.grants[grant_free] = Grant {
            active: true,
            generation: grant_generation,
            parent_slot: source.grant_slot,
            parent_generation: source.grant_generation,
        };
        let target_object = if let (Some(scope_index), Some((target, is_directory, owner_pid))) =
            (scope_free, narrowed_scope.as_ref())
        {
            let generation = next_generation(state.scopes[scope_index].generation);
            let mut scope = VfsScope {
                active: true,
                generation,
                owner_pid: *owner_pid,
                refs: 0,
                is_directory: *is_directory,
                path_len: target.len() as u8,
                path: [0; vfs::PATH_MAX],
            };
            scope.path[..target.len()].copy_from_slice(target.as_bytes());
            state.scopes[scope_index] = scope;
            ObjectRef {
                kind: ObjectKind::VfsScope,
                slot: scope_index as u16,
                generation,
            }
        } else {
            source.object
        };
        let target_token = state.install_cap(
            target_process,
            target_cap_index,
            target_object,
            requested_rights,
            (grant_free as u16, grant_generation),
        );
        let revoker_token = next_token();
        state.processes[source_process].caps[revoker_index] = CapSlot {
            active: true,
            revoked: false,
            generation: next_generation(
                state.processes[source_process].caps[revoker_index].generation,
            ),
            token: revoker_token,
            object: ObjectRef {
                kind: ObjectKind::Revoker,
                slot: grant_free as u16,
                generation: grant_generation,
            },
            rights: RIGHT_CLOSE,
            grant_slot: NO_INDEX,
            grant_generation: 0,
        };
        // Capacity was checked above while the same lock was held, so this
        // cannot fail and no partially-published delegation needs rollback.
        let published = state.processes[target_process].push_inbox(target_token);
        debug_assert!(published);
        if state.processes[target_process].accept_waiter != 0 {
            let waiter = state.processes[target_process].accept_waiter;
            state.processes[target_process].accept_waiter = 0;
            task::wake_ipc_task(waiter);
        }
        Ok(revoker_token as i64)
    })
    .unwrap_or_else(|error| error)
}

pub fn sys_capability_accept(ptr: u64, len: u64) -> i64 {
    let bytes = match read_user_fixed::<ACCEPT_V1_SIZE>(ptr, len) {
        Ok(bytes) => bytes,
        Err(error) => return error,
    };
    if let Err(error) = header(&bytes, ACCEPT_V1_SIZE) {
        return error;
    }
    let timeout = match le_u64(&bytes, 8) {
        Ok(value) if value <= MAX_TIMEOUT_TICKS => value,
        _ => return ERR_BAD_MESSAGE,
    };
    let end = if timeout == 0 {
        u64::MAX
    } else {
        match crate::interrupts::ticks().checked_add(timeout) {
            Some(value) => value,
            None => return ERR_BAD_MESSAGE,
        }
    };
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    loop {
        let result = with_state(|state| {
            let process_index = state.ensure_process(pid)?;
            if let Some(token) = state.processes[process_index].pop_inbox() {
                return Ok(token as i64);
            }
            if expired(end) {
                state.processes[process_index].accept_waiter = 0;
                return Err(ERR_TIMEOUT);
            }
            state.processes[process_index].accept_waiter = pid;
            task::block_current_for_ipc(end);
            Ok(i64::MIN)
        });
        match result {
            Ok(value) if value != i64::MIN => return value,
            Ok(_) => task::schedule(),
            Err(error) => return error,
        }
    }
}

fn private_root() -> Result<String, i64> {
    let name = task::current_process_name().ok_or(ERR_INVALID)?;
    let mut root = String::from("/data/");
    root.try_reserve_exact(name.len())
        .map_err(|_| ERR_EXHAUSTED)?;
    root.push_str(&name);
    Ok(root)
}

pub fn sys_fs_scope_create(ptr: u64, len: u64) -> i64 {
    let bytes = match read_user_fixed::<FS_SCOPE_V1_SIZE>(ptr, len) {
        Ok(bytes) => bytes,
        Err(error) => return error,
    };
    if let Err(error) = header(&bytes, FS_SCOPE_V1_SIZE) {
        return error;
    }
    let rights = match le_u32(&bytes, 8) {
        Ok(rights)
            if rights != 0
                && rights & !FILE_RIGHTS == 0
                && rights & (RIGHT_FILE_READ | RIGHT_FILE_WRITE | RIGHT_FILE_LIST) != 0 =>
        {
            rights
        }
        _ => return ERR_RIGHTS,
    };
    let path_len = match le_u16(&bytes, 12) {
        Ok(value) if usize::from(value) <= vfs::PATH_MAX => usize::from(value),
        _ => return ERR_INVALID,
    };
    if le_u16(&bytes, 14).unwrap_or(1) != 0 {
        return ERR_BAD_MESSAGE;
    }
    let path = match core::str::from_utf8(&bytes[16..16 + path_len]) {
        Ok(path) => path,
        Err(_) => return ERR_INVALID,
    };
    let cwd = match task::current_working_directory() {
        Some(cwd) => cwd,
        None => return ERR_INVALID,
    };
    let absolute = match vfs::normalize(&cwd, path) {
        Ok(path) => path,
        Err(_) => return ERR_INVALID,
    };
    let root = match private_root() {
        Ok(root) => root,
        Err(error) => return error,
    };
    if !path_is_within(&root, &absolute, true) {
        return ERR_RIGHTS;
    }
    let kind = match vfs::kind("/", &absolute) {
        Ok(kind) => kind,
        Err(_) => return ERR_INVALID,
    };
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    with_state(|state| {
        let process_index = state.ensure_process(pid)?;
        let cap_index = state.processes[process_index]
            .free_cap()
            .ok_or(ERR_EXHAUSTED)?;
        let scope_index = state
            .scopes
            .iter()
            .position(|scope| !scope.active)
            .ok_or(ERR_EXHAUSTED)?;
        let grant = state.allocate_grant(None)?;
        let scope_generation = next_generation(state.scopes[scope_index].generation);
        let mut scope = VfsScope {
            active: true,
            generation: scope_generation,
            owner_pid: pid,
            refs: 0,
            is_directory: kind == vfs::NodeKind::Directory,
            path_len: absolute.len() as u8,
            path: [0; vfs::PATH_MAX],
        };
        scope.path[..absolute.len()].copy_from_slice(absolute.as_bytes());
        state.scopes[scope_index] = scope;
        let token = state.install_cap(
            process_index,
            cap_index,
            ObjectRef {
                kind: ObjectKind::VfsScope,
                slot: scope_index as u16,
                generation: scope_generation,
            },
            rights,
            grant,
        );
        Ok::<i64, i64>(token as i64)
    })
    .unwrap_or_else(|error| error)
}

struct FsRequest {
    handle: u64,
    path: String,
    data_ptr: u64,
    data_len: usize,
    out_ptr: u64,
    out_len: usize,
}

fn parse_fs_request(ptr: u64, len: u64) -> Result<FsRequest, i64> {
    let bytes = read_user_fixed::<FS_REQUEST_V1_SIZE>(ptr, len)?;
    header(&bytes, FS_REQUEST_V1_SIZE)?;
    let path_len = usize::from(le_u16(&bytes, 16)?);
    if path_len > vfs::PATH_MAX || le_u16(&bytes, 18)? != 0 || le_u32(&bytes, 20)? != 0 {
        return Err(ERR_BAD_MESSAGE);
    }
    let text = core::str::from_utf8(&bytes[56..56 + path_len]).map_err(|_| ERR_INVALID)?;
    let mut path = String::new();
    path.try_reserve_exact(path_len)
        .map_err(|_| ERR_EXHAUSTED)?;
    path.push_str(text);
    let data_len = usize::try_from(le_u64(&bytes, 32)?).map_err(|_| ERR_INVALID)?;
    let out_len = usize::try_from(le_u64(&bytes, 48)?).map_err(|_| ERR_INVALID)?;
    Ok(FsRequest {
        handle: le_u64(&bytes, 8)?,
        path,
        data_ptr: le_u64(&bytes, 24)?,
        data_len,
        out_ptr: le_u64(&bytes, 40)?,
        out_len,
    })
}

pub fn sys_fs_read(ptr: u64, len: u64) -> i64 {
    let request = match parse_fs_request(ptr, len) {
        Ok(request) => request,
        Err(error) => return error,
    };
    if request.data_ptr != 0
        || request.data_len != 0
        || request.out_len == 0
        || request.out_len > 4096
    {
        return ERR_BAD_MESSAGE;
    }
    if !task::validate_current_user_range(request.out_ptr, request.out_len, true) {
        return ERR_INVALID;
    }
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    let view = match scope_view(pid, request.handle, RIGHT_FILE_READ) {
        Ok(view) => view,
        Err(error) => return error,
    };
    let target = match resolve_scope_path(&view, &request.path) {
        Ok(target) => target,
        Err(error) => return error,
    };
    let data = match vfs::read_file("/", &target) {
        Ok(data) => data,
        Err(_) => return ERR_INVALID,
    };
    if data.len() > request.out_len {
        return ERR_EXHAUSTED;
    }
    if !task::copy_to_current_user(request.out_ptr, &data) {
        return ERR_INVALID;
    }
    data.len() as i64
}

pub fn sys_fs_put(ptr: u64, len: u64) -> i64 {
    let request = match parse_fs_request(ptr, len) {
        Ok(request) => request,
        Err(error) => return error,
    };
    if request.data_len == 0
        || request.data_len > 4096
        || request.out_ptr != 0
        || request.out_len != 0
    {
        return ERR_BAD_MESSAGE;
    }
    let data = match task::copy_from_current_user(request.data_ptr, request.data_len) {
        Some(data) => data,
        None => return ERR_INVALID,
    };
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    let view = match scope_view(pid, request.handle, RIGHT_FILE_WRITE) {
        Ok(view) => view,
        Err(error) => return error,
    };
    let target = match resolve_scope_path(&view, &request.path) {
        Ok(target) => target,
        Err(error) => return error,
    };
    vfs::write_file("/", &target, &data)
        .map(|_| OK)
        .unwrap_or(ERR_INVALID)
}

pub fn sys_fs_list(ptr: u64, len: u64) -> i64 {
    let request = match parse_fs_request(ptr, len) {
        Ok(request) => request,
        Err(error) => return error,
    };
    if request.data_ptr != 0
        || request.data_len != 0
        || request.out_len == 0
        || request.out_len > 4096
    {
        return ERR_BAD_MESSAGE;
    }
    if !task::validate_current_user_range(request.out_ptr, request.out_len, true) {
        return ERR_INVALID;
    }
    let pid = match current_pid() {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    let view = match scope_view(pid, request.handle, RIGHT_FILE_LIST) {
        Ok(view) => view,
        Err(error) => return error,
    };
    let target = match resolve_scope_path(&view, &request.path) {
        Ok(target) => target,
        Err(error) => return error,
    };
    let entries = match vfs::list_dir("/", &target) {
        Ok(entries) => entries,
        Err(_) => return ERR_INVALID,
    };
    let mut encoded = alloc::vec::Vec::new();
    if encoded.try_reserve_exact(request.out_len).is_err() {
        return ERR_EXHAUSTED;
    }
    for entry in entries {
        let Some(required) = encoded
            .len()
            .checked_add(entry.len())
            .and_then(|value| value.checked_add(1))
        else {
            return ERR_INVALID;
        };
        if required > request.out_len {
            return ERR_EXHAUSTED;
        }
        encoded.extend_from_slice(entry.as_bytes());
        encoded.push(b'\n');
    }
    if !encoded.is_empty() && !task::copy_to_current_user(request.out_ptr, &encoded) {
        return ERR_INVALID;
    }
    encoded.len() as i64
}

pub fn process_exit(pid: u32) {
    with_state(|state| {
        for index in 0..MAX_ENDPOINTS {
            if state.endpoints[index].active && state.endpoints[index].owner_pid == pid {
                state.close_endpoint_index(index, ERR_PEER_EXITED);
            }
        }
        for scope in state.scopes.iter_mut() {
            if scope.active && scope.owner_pid == pid {
                scope.active = false;
            }
        }
        // Remove requests whose caller died before a provider received them.
        // Keep the endpoint/correlation pairs in fixed storage so queue
        // surgery occurs after the mutable call-table walk.
        let mut abandoned = [(ObjectRef::NONE, 0u64); MAX_CALLS];
        let mut abandoned_count = 0usize;
        for call in state.calls.iter_mut() {
            if call.active && call.caller_pid == pid {
                abandoned[abandoned_count] = (call.endpoint, call.token);
                abandoned_count += 1;
                call.active = false;
            } else if call.active && call.authorized_pid == pid && call.state == CallState::Waiting
            {
                call.state = CallState::Failed;
                call.error = ERR_PEER_EXITED;
                task::wake_ipc_task(call.caller_pid);
            }
        }
        for (endpoint, token) in abandoned[..abandoned_count].iter().copied() {
            state.remove_queued_correlation(endpoint, token);
        }
        for endpoint in state.endpoints.iter_mut() {
            IpcState::remove_waiter(
                &mut endpoint.send_waiters,
                &mut endpoint.send_waiter_count,
                pid,
            );
            IpcState::remove_waiter(
                &mut endpoint.receive_waiters,
                &mut endpoint.receive_waiter_count,
                pid,
            );
        }
        if let Some(process_index) = state.process_index(pid) {
            for cap_index in 0..MAX_CAPS_PER_PROCESS {
                let cap = state.processes[process_index].caps[cap_index];
                if cap.active {
                    if cap.object.kind == ObjectKind::Revoker {
                        state.revoke_grant_tree(cap.object.slot, cap.object.generation);
                    } else {
                        state.drop_object_ref(cap.object);
                        state.revoke_grant_tree(cap.grant_slot, cap.grant_generation);
                    }
                }
                state.processes[process_index].caps[cap_index] = CapSlot::EMPTY;
            }
            state.processes[process_index] = ProcessCaps::EMPTY;
        }
    });
}

#[derive(Clone, Copy)]
pub struct Stats {
    pub endpoints: usize,
    pub capabilities: usize,
    pub calls: usize,
    pub scopes: usize,
    pub queued_messages: usize,
    pub waiters: usize,
}

pub fn stats() -> Stats {
    with_state(|state| Stats {
        endpoints: state
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.active)
            .count(),
        capabilities: state
            .processes
            .iter()
            .flat_map(|process| process.caps.iter())
            .filter(|cap| cap.active && !cap.revoked)
            .count(),
        calls: state.calls.iter().filter(|call| call.active).count(),
        scopes: state.scopes.iter().filter(|scope| scope.active).count(),
        queued_messages: state
            .endpoints
            .iter()
            .map(|endpoint| usize::from(endpoint.count))
            .sum(),
        waiters: state
            .endpoints
            .iter()
            .map(|endpoint| {
                usize::from(endpoint.send_waiter_count) + usize::from(endpoint.receive_waiter_count)
            })
            .sum::<usize>()
            + state
                .processes
                .iter()
                .filter(|process| process.accept_waiter != 0)
                .count(),
    })
}

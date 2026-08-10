//! Unified Ring 3 input-event queue (Phase 5) -- keyboard and mouse events
//! merged into one bounded queue a foreground desktop can drain via
//! `SYS_INPUT_POLL`.
//!
//! Keyboard events are routed exclusively: `keyboard.rs` sends each decoded
//! key either to the privileged shell or here, never both. Foreground
//! transitions clear this queue and the shell queue before changing owner,
//! preventing delayed desktop keystrokes from replaying as shell commands.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use lazy_static::lazy_static;
use spin::Mutex;

use crate::keyboard::KeyEvent;

/// Same bounded-queue policy as `keyboard::QUEUE` (`keyboard.rs`'s own
/// docs): silently drop new events once full rather than block the
/// producer (an ISR) or grow without limit. A desktop process is expected
/// to drain this every frame, so staying full for any length of time
/// would only happen if nothing is polling at all -- at which point
/// dropping is exactly the right behavior.
const QUEUE_CAPACITY: usize = 64;

/// One decoded input event. Encoded as a fixed 8-byte little-endian record
/// for `SYS_INPUT_POLL` (see `to_le_bytes`) -- deliberately small and
/// fixed-size so the syscall boundary never needs a variable-length,
/// unbounded user buffer.
#[derive(Clone, Copy)]
pub enum InputEvent {
    /// A key was pressed. `code` is the value `to_le_bytes` puts in byte 1
    /// -- either the ASCII byte itself (for `KeyEvent::Char`) or one of the
    /// small control-byte codes below for the handful of non-character
    /// keys this project's keyboard driver decodes. There is no `KeyUp`:
    /// `keyboard.rs`'s scancode decoder does not track release events for
    /// ordinary keys (only for the Shift modifiers, internally) -- adding
    /// full per-key press/release tracking is future work, not implemented
    /// here, and this is documented rather than faked with a synthetic
    /// release that never actually happens.
    KeyDown { code: u8 },
    KeyUp { code: u8 },
    /// Absolute, screen-clamped cursor position (see `mouse.rs`), not a
    /// delta -- the kernel already tracks and clamps cursor position, so a
    /// desktop process never needs to replicate that bookkeeping or risk
    /// drawing a cursor off-screen.
    MouseMove { x: i16, y: i16 },
    /// `button`: 0 = left, 1 = right, 2 = middle. `pressed`: `true` on
    /// press, `false` on release -- unlike keyboard events, the PS/2 mouse
    /// protocol reports button state on every packet, so release events
    /// are genuinely available here (see `mouse.rs`) and are unconditionally
    /// captured, not just presses.
    MouseButton { button: u8, pressed: bool },
}

/// Control-byte codes `InputEvent::KeyDown` uses for keys that don't have
/// an ordinary printable ASCII value -- chosen from the C0 control range,
/// clear of any printable character `keyboard::KeyEvent::Char` could ever
/// carry (0x20..=0x7E).
pub const KEY_ENTER: u8 = 0x0D;
pub const KEY_BACKSPACE: u8 = 0x08;
pub const KEY_TAB: u8 = 0x09;
pub const KEY_ARROW_UP: u8 = 0x11;
pub const KEY_ARROW_DOWN: u8 = 0x12;
pub const KEY_ESCAPE: u8 = 0x1B;

/// Byte length of `InputEvent::to_le_bytes`'s output -- what
/// `syscall::sys_input_poll` checks the caller's destination buffer against
/// before ever touching it.
pub const ENCODED_EVENT_LEN: usize = 8;

impl InputEvent {
    /// Fixed 8-byte encoding: `[tag, a, b, pad, x_lo, x_hi, y_lo, y_hi]`.
    /// `tag`: 1 = KeyDown, 2 = MouseMove, 3 = MouseButton. Unused fields
    /// for a given tag are zeroed, not left undefined, so a consumer that
    /// (incorrectly) reads them anyway still gets a deterministic value
    /// rather than stale/uninitialized-looking bytes.
    pub fn to_le_bytes(self) -> [u8; ENCODED_EVENT_LEN] {
        let mut out = [0u8; ENCODED_EVENT_LEN];
        match self {
            InputEvent::KeyDown { code } => {
                out[0] = 1;
                out[1] = code;
            }
            InputEvent::KeyUp { code } => {
                out[0] = 4;
                out[1] = code;
            }
            InputEvent::MouseMove { x, y } => {
                out[0] = 2;
                out[4..6].copy_from_slice(&x.to_le_bytes());
                out[6..8].copy_from_slice(&y.to_le_bytes());
            }
            InputEvent::MouseButton { button, pressed } => {
                out[0] = 3;
                out[1] = button;
                out[2] = pressed as u8;
            }
        }
        out
    }
}

#[derive(Clone, Copy)]
struct QueuedInputEvent {
    event: InputEvent,
    queued_at_tick: u64,
}

/// Snapshot of bounded-queue behavior for QEMU acceptance telemetry.
#[derive(Clone, Copy)]
pub struct InputTelemetry {
    pub current_depth: usize,
    pub max_depth: usize,
    pub coalesced_mouse_moves: u64,
    pub dropped_events: u64,
    pub delivered_mouse_moves: u64,
    pub mouse_age_total_ticks: u64,
    pub mouse_age_max_ticks: u64,
}

static MAX_DEPTH: AtomicUsize = AtomicUsize::new(0);
static COALESCED_MOUSE_MOVES: AtomicU64 = AtomicU64::new(0);
static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);
static DELIVERED_MOUSE_MOVES: AtomicU64 = AtomicU64::new(0);
static MOUSE_AGE_TOTAL_TICKS: AtomicU64 = AtomicU64::new(0);
static MOUSE_AGE_MAX_TICKS: AtomicU64 = AtomicU64::new(0);

lazy_static! {
    static ref QUEUE: Mutex<VecDeque<QueuedInputEvent>> =
        Mutex::new(VecDeque::with_capacity(QUEUE_CAPACITY));
}

/// The only sanctioned way to touch `QUEUE` -- identical reasoning and
/// identical pattern to `keyboard::with_queue`/`task::with_scheduler`:
/// `push` runs from interrupt context (keyboard and mouse ISRs both feed
/// this queue), and `poll` runs from ordinary syscall context. The lock is
/// independently IRQ-safe even though long VM syscalls elsewhere may enable
/// interrupts between their own bounded transactions -- without
/// disabling interrupts for the critical section, an IRQ landing at the
/// exact moment the other side held the lock would deadlock the same way
/// every other shared-queue bug in this codebase already did, and was
/// fixed, before this module existed.
fn with_queue<F, R>(f: F) -> R
where
    F: FnOnce(&mut VecDeque<QueuedInputEvent>) -> R,
{
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut guard = QUEUE.lock();
        f(&mut guard)
    })
}

/// Push one event in from an ISR (keyboard or mouse). Must stay fast and
/// non-blocking, same requirement as `keyboard::push` -- this only ever
/// does a bounded `VecDeque` push, never allocates on the hot path beyond
/// the queue's own pre-reserved capacity, and never blocks.
pub fn push(event: InputEvent) {
    if crate::keyboard::foreground_process_id().is_none() {
        return;
    }

    let queued = QueuedInputEvent {
        event,
        queued_at_tick: crate::interrupts::ticks(),
    };
    let queued_or_coalesced = with_queue(|queue| {
        // Mouse packets commonly arrive in bursts faster than a full-screen
        // userspace redraw. Only the newest position matters until an event
        // of another kind establishes an ordering boundary. Replacing an
        // adjacent move keeps button edges and key ordering exact while
        // bounding cursor lag to one queued position.
        if matches!(event, InputEvent::MouseMove { .. }) {
            if let Some(last) = queue.back_mut() {
                if matches!(last.event, InputEvent::MouseMove { .. }) {
                    *last = queued;
                    COALESCED_MOUSE_MOVES.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
            }
        }
        if queue.len() < QUEUE_CAPACITY {
            queue.push_back(queued);
            update_max(&MAX_DEPTH, queue.len());
            true
        } else {
            DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
            false
        }
    });
    if queued_or_coalesced {
        if let Some(id) = crate::keyboard::foreground_process_id() {
            crate::task::request_foreground_wake(id);
        }
    }
}

/// Drain one queued event, or `None` if empty -- `SYS_INPUT_POLL`'s
/// underlying primitive.
pub fn poll() -> Option<InputEvent> {
    let queued = with_queue(|queue| queue.pop_front())?;
    if matches!(queued.event, InputEvent::MouseMove { .. }) {
        let age = crate::interrupts::ticks().saturating_sub(queued.queued_at_tick);
        DELIVERED_MOUSE_MOVES.fetch_add(1, Ordering::Relaxed);
        MOUSE_AGE_TOTAL_TICKS.fetch_add(age, Ordering::Relaxed);
        update_max_u64(&MOUSE_AGE_MAX_TICKS, age);
    }
    Some(queued.event)
}

/// Discard all queued Ring 3 events. Foreground handoff is the normal caller;
/// this is public within the kernel so that handoff can clear both queues
/// while interrupts remain disabled across the complete transition.
pub(crate) fn clear() {
    with_queue(|queue| queue.clear());
}

/// Begin a fresh foreground-session measurement. Called while interrupts
/// remain disabled during the shell-to-desktop handoff.
pub(crate) fn reset_telemetry() {
    MAX_DEPTH.store(0, Ordering::Relaxed);
    COALESCED_MOUSE_MOVES.store(0, Ordering::Relaxed);
    DROPPED_EVENTS.store(0, Ordering::Relaxed);
    DELIVERED_MOUSE_MOVES.store(0, Ordering::Relaxed);
    MOUSE_AGE_TOTAL_TICKS.store(0, Ordering::Relaxed);
    MOUSE_AGE_MAX_TICKS.store(0, Ordering::Relaxed);
}

pub fn telemetry() -> InputTelemetry {
    InputTelemetry {
        current_depth: with_queue(|queue| queue.len()),
        max_depth: MAX_DEPTH.load(Ordering::Relaxed),
        coalesced_mouse_moves: COALESCED_MOUSE_MOVES.load(Ordering::Relaxed),
        dropped_events: DROPPED_EVENTS.load(Ordering::Relaxed),
        delivered_mouse_moves: DELIVERED_MOUSE_MOVES.load(Ordering::Relaxed),
        mouse_age_total_ticks: MOUSE_AGE_TOTAL_TICKS.load(Ordering::Relaxed),
        mouse_age_max_ticks: MOUSE_AGE_MAX_TICKS.load(Ordering::Relaxed),
    }
}

fn update_max(target: &AtomicUsize, value: usize) {
    let mut current = target.load(Ordering::Relaxed);
    while value > current {
        match target.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

fn update_max_u64(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Relaxed);
    while value > current {
        match target.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

/// Translate an already-decoded `keyboard::KeyEvent` into this module's
/// `InputEvent::KeyDown` and push it, if it maps to one at all --
/// `KeyEvent::None` (nothing decoded from this scancode, e.g. a bare Shift
/// press/release) has nothing to push. Called only by `keyboard.rs` when
/// foreground ownership is `Desktop`; the same event is never also placed
/// in the shell queue.
pub fn push_key_event(event: KeyEvent) {
    let code = match event {
        KeyEvent::Char(c) => c,
        KeyEvent::Enter => KEY_ENTER,
        KeyEvent::Backspace => KEY_BACKSPACE,
        KeyEvent::Tab => KEY_TAB,
        KeyEvent::ArrowUp => KEY_ARROW_UP,
        KeyEvent::ArrowDown => KEY_ARROW_DOWN,
        KeyEvent::Escape => KEY_ESCAPE,
        KeyEvent::KeyUp(scancode) => {
            push(InputEvent::KeyUp { code: scancode });
            return;
        }
        KeyEvent::None => return,
    };
    push(InputEvent::KeyDown { code });
}

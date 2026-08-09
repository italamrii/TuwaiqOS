//! PS/2 keyboard input (interrupt-driven).
//!
//! Scancodes arrive one byte at a time from `interrupts::keyboard_interrupt_handler`
//! (IRQ1), are decoded here into `KeyEvent`s, and routed to exactly one
//! foreground owner: either the Ring 0 shell queue or the Ring 3 input queue.
//! Ownership changes clear both queues, so keys typed into the desktop can
//! never replay into the privileged shell after the desktop exits.
//!
//! Supports printable keys, Shift modifiers, arrow keys, and Tab.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::instructions::port::Port;

/// Detect a legacy PS/2 controller with the standard controller self-test.
/// The transaction is bounded, runs with interrupts masked so IRQ1 cannot
/// consume the reply, and re-enables the first port after a successful test.
pub fn probe_controller() -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut status_port = Port::<u8>::new(0x64);
        let mut command_port = Port::<u8>::new(0x64);
        let mut data_port = Port::<u8>::new(0x60);

        for _ in 0..1_024 {
            let status = unsafe { status_port.read() };
            if status & 1 == 0 {
                break;
            }
            let _ = unsafe { data_port.read() };
        }
        let mut input_ready = false;
        for _ in 0..1_024 {
            let status = unsafe { status_port.read() };
            if status != 0xFF && status & 2 == 0 {
                input_ready = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !input_ready {
            return false;
        }
        unsafe { command_port.write(0xAA) };
        for _ in 0..4_096 {
            let status = unsafe { status_port.read() };
            if status & 1 != 0 {
                let passed = unsafe { data_port.read() } == 0x55;
                if passed {
                    unsafe { command_port.write(0xAE) };
                }
                return passed;
            }
            core::hint::spin_loop();
        }
        false
    })
}

/// A decoded keyboard event for the shell.
#[derive(Clone, Copy)]
pub enum KeyEvent {
    Char(u8),
    Enter,
    Backspace,
    ArrowUp,
    ArrowDown,
    Tab,
    Escape,
    None,
}

/// The one destination allowed to receive decoded keyboard events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ForegroundInputOwner {
    Shell = 0,
    Desktop = 1,
}

static LEFT_SHIFT: AtomicBool = AtomicBool::new(false);
static RIGHT_SHIFT: AtomicBool = AtomicBool::new(false);
/// Set after seeing the 0xE0 extended-scancode prefix; the *next* byte
/// delivered by IRQ1 completes that two-byte sequence.
static EXTENDED_PENDING: AtomicBool = AtomicBool::new(false);
/// PID 0 is reserved as the Ring 0 shell sentinel; scheduler user-task IDs
/// start above zero. A nonzero value binds input polling to that one process.
static FOREGROUND_PROCESS_ID: AtomicU32 = AtomicU32::new(0);

fn shift_active() -> bool {
    LEFT_SHIFT.load(Ordering::Relaxed) || RIGHT_SHIFT.load(Ordering::Relaxed)
}

const QUEUE_CAPACITY: usize = 32;

lazy_static! {
    static ref QUEUE: Mutex<VecDeque<KeyEvent>> =
        Mutex::new(VecDeque::with_capacity(QUEUE_CAPACITY));
}

/// The only sanctioned way to touch `QUEUE`. `push` runs inside the
/// keyboard ISR (interrupts already disabled by the CPU); `poll_key` runs
/// in ordinary shell context with interrupts enabled. Without disabling
/// interrupts here, a keyboard IRQ landing at the exact moment `poll_key`
/// held this lock would deadlock: `on_scancode`'s own attempt to lock
/// `QUEUE` inside the ISR would spin forever waiting for a lock that can
/// only be released by `poll_key` finishing -- which can't happen until
/// the ISR itself returns via `iretq`. Same bug class, and same fix, as
/// `task::with_scheduler` and the interrupt-safe heap allocator.
fn with_queue<F, R>(f: F) -> R
where
    F: FnOnce(&mut VecDeque<KeyEvent>) -> R,
{
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut guard = QUEUE.lock();
        f(&mut guard)
    })
}

/// Feed one raw scancode byte in from the keyboard ISR. Runs with
/// interrupts disabled (we're inside the ISR), so this must stay fast and
/// must never block.
pub fn on_scancode(scancode: u8) {
    if EXTENDED_PENDING.swap(false, Ordering::Relaxed) {
        if let Some(event) = translate_extended(scancode) {
            route(event);
        }
        return;
    }

    if scancode == 0xE0 {
        EXTENDED_PENDING.store(true, Ordering::Relaxed);
        return;
    }

    if let Some(event) = translate_scancode(scancode) {
        route(event);
    }
}

fn route(event: KeyEvent) {
    match foreground_owner() {
        ForegroundInputOwner::Shell => push_shell(event),
        ForegroundInputOwner::Desktop => crate::input::push_key_event(event),
    }
}

fn push_shell(event: KeyEvent) {
    with_queue(|queue| {
        if queue.len() < QUEUE_CAPACITY {
            queue.push_back(event);
        }
        // Silently drop when full: better to lose an unread keystroke than
        // to block the ISR or grow the queue unbounded.
    });
}

/// Return the current exclusive keyboard-input owner.
pub fn foreground_owner() -> ForegroundInputOwner {
    if foreground_process_id().is_some() {
        ForegroundInputOwner::Desktop
    } else {
        ForegroundInputOwner::Shell
    }
}

/// The only Ring 3 PID allowed to consume foreground input, or `None` while
/// the privileged shell owns the keyboard.
pub fn foreground_process_id() -> Option<u32> {
    match FOREGROUND_PROCESS_ID.load(Ordering::Acquire) {
        0 => None,
        pid => Some(pid),
    }
}

/// Atomically hand keyboard input to `owner` and discard every event queued
/// for either the previous or next owner. Interrupts stay disabled across
/// both queue clears and the owner store, so an IRQ1 cannot land in the
/// middle and route a key according to half-transitioned state.
pub fn set_foreground_process_id(process_id: Option<u32>) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        QUEUE.lock().clear();
        crate::input::clear();
        if process_id.is_some() {
            crate::input::reset_telemetry();
        }
        // Do not let an 0xE0 prefix received by one owner reinterpret the
        // first byte received by the next owner.
        EXTENDED_PENDING.store(false, Ordering::Relaxed);
        FOREGROUND_PROCESS_ID.store(process_id.unwrap_or(0), Ordering::Release);
    });
}

/// Drain one queued key event. Returns `KeyEvent::None` immediately if
/// nothing is waiting -- callers that want to idle instead of spin should
/// call `interrupts::halt()` on `None` (see `shell::run`).
pub fn poll_key() -> KeyEvent {
    with_queue(|queue| queue.pop_front()).unwrap_or(KeyEvent::None)
}

fn translate_extended(scancode: u8) -> Option<KeyEvent> {
    if scancode & 0x80 != 0 {
        return None;
    }
    match scancode {
        0x48 => Some(KeyEvent::ArrowUp),
        0x50 => Some(KeyEvent::ArrowDown),
        _ => None,
    }
}

fn translate_scancode(scancode: u8) -> Option<KeyEvent> {
    if scancode & 0x80 != 0 {
        match scancode {
            0xAA => LEFT_SHIFT.store(false, Ordering::Relaxed),
            0xB6 => RIGHT_SHIFT.store(false, Ordering::Relaxed),
            _ => {}
        }
        return None;
    }

    match scancode {
        0x2A => {
            LEFT_SHIFT.store(true, Ordering::Relaxed);
            None
        }
        0x36 => {
            RIGHT_SHIFT.store(true, Ordering::Relaxed);
            None
        }
        0x1C => Some(KeyEvent::Enter),
        0x0E => Some(KeyEvent::Backspace),
        0x0F => Some(KeyEvent::Tab),
        0x01 => Some(KeyEvent::Escape),
        0x39 => Some(KeyEvent::Char(b' ')),
        0x02 => emit_pair(b'1', b'!'),
        0x03 => emit_pair(b'2', b'@'),
        0x04 => emit_pair(b'3', b'#'),
        0x05 => emit_pair(b'4', b'$'),
        0x06 => emit_pair(b'5', b'%'),
        0x07 => emit_pair(b'6', b'^'),
        0x08 => emit_pair(b'7', b'&'),
        0x09 => emit_pair(b'8', b'*'),
        0x0A => emit_pair(b'9', b'('),
        0x0B => emit_pair(b'0', b')'),
        0x0C => emit_pair(b'-', b'_'),
        0x0D => emit_pair(b'=', b'+'),
        0x29 => emit_pair(b'`', b'~'),
        0x1A => emit_pair(b'[', b'{'),
        0x1B => emit_pair(b']', b'}'),
        0x2B => emit_pair(b'\\', b'|'),
        0x27 => emit_pair(b';', b':'),
        0x28 => emit_pair(b'\'', b'"'),
        0x33 => emit_pair(b',', b'<'),
        0x34 => emit_pair(b'.', b'>'),
        0x35 => emit_pair(b'/', b'?'),
        0x10 => emit_letter(b'q'),
        0x11 => emit_letter(b'w'),
        0x12 => emit_letter(b'e'),
        0x13 => emit_letter(b'r'),
        0x14 => emit_letter(b't'),
        0x15 => emit_letter(b'y'),
        0x16 => emit_letter(b'u'),
        0x17 => emit_letter(b'i'),
        0x18 => emit_letter(b'o'),
        0x19 => emit_letter(b'p'),
        0x1E => emit_letter(b'a'),
        0x1F => emit_letter(b's'),
        0x20 => emit_letter(b'd'),
        0x21 => emit_letter(b'f'),
        0x22 => emit_letter(b'g'),
        0x23 => emit_letter(b'h'),
        0x24 => emit_letter(b'j'),
        0x25 => emit_letter(b'k'),
        0x26 => emit_letter(b'l'),
        0x2C => emit_letter(b'z'),
        0x2D => emit_letter(b'x'),
        0x2E => emit_letter(b'c'),
        0x2F => emit_letter(b'v'),
        0x30 => emit_letter(b'b'),
        0x31 => emit_letter(b'n'),
        0x32 => emit_letter(b'm'),
        _ => None,
    }
}

fn emit_pair(normal: u8, shifted: u8) -> Option<KeyEvent> {
    Some(KeyEvent::Char(if shift_active() {
        shifted
    } else {
        normal
    }))
}

fn emit_letter(lower: u8) -> Option<KeyEvent> {
    Some(KeyEvent::Char(if shift_active() {
        lower - b'a' + b'A'
    } else {
        lower
    }))
}

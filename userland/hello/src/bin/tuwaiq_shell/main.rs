//! Tuwaiq Shell: an interface identity prototype.
//!
//! A separate Ring 3 program rather than a change to `desktop`. The existing
//! desktop is covered by `scripts/phase5-acceptance.ps1` and keeping it
//! byte-identical means bold interface ideas can be tried without putting any
//! of that at risk -- run one or the other and compare.
//!
//! It reuses `desktop/gfx.rs`, `desktop/font.rs` and `desktop/sys.rs` verbatim
//! by path rather than copying them, so the rendering and syscall paths under
//! test stay the ones already in use. Everything new lives in this directory.
//!
//! The idea in one line: Tuwaiq is a plateau, one sharp break, and a lower
//! plain, and that silhouette is the shape of the panel rather than a picture
//! hung behind it. See `escarpment.rs` for the profile every surface shares.
//!
//! Controls
//!   click the mark, or A   raise the launcher
//!   click the tray, or S   pull the console
//!   N                      post a notification
//!   L                      swap the light and dark themes
//!   Esc                    exit
//!
//! Nothing here is privileged. Like `desktop` it draws into its own
//! `SYS_MMAP`ed backbuffer and reaches the screen only through
//! `SYS_DISPLAY_PRESENT`.

#![no_std]
#![no_main]

#[path = "../desktop/font.rs"]
mod font;
#[path = "../desktop/gfx.rs"]
mod gfx;
#[path = "../desktop/sys.rs"]
mod sys;

mod console;
mod escarpment;
mod ridge;
mod theme;

use core::arch::global_asm;

use gfx::Canvas;
use theme::{Theme, DARK, LIGHT};

global_asm!(
    r#"
.global _start
_start:
    call {main}
1:
    jmp 1b
"#,
    main = sym rust_main,
);

/// How long a notification stays on the ledge, in timer ticks (100 Hz).
const TOAST_TICKS: u64 = 260;

/// A closed panel. Any real tick is smaller, so this doubles as the sentinel.
const CLOSED: u64 = u64::MAX;

struct Shell {
    cursor_x: i32,
    cursor_y: i32,
    dark: bool,
    /// Tick the launcher was asked to open on, or `CLOSED`
    ///
    /// Animation is driven by the clock rather than by frame count. The
    /// framebuffer path here renders every pixel through a bounds-checked
    /// `put_pixel`, so a full 1280x720 frame is expensive and the frame rate
    /// is neither high nor steady. Counting frames made a 340 ms rise take
    /// well over a second and vary with what else the system was doing.
    ridge_at: u64,
    console_at: u64,
    toast_at: u64,
    bands: [console::Band; console::COUNT],
    hovered_app: Option<usize>,
    frame: u64,
    /// Current tick, sampled once per frame so every animation in one frame
    /// agrees on the time
    now: u64,
}

impl Shell {
    fn theme(&self) -> &'static Theme {
        if self.dark {
            &DARK
        } else {
            &LIGHT
        }
    }

    fn ridge_open(&self) -> bool {
        self.ridge_at != CLOSED
    }

    fn console_open(&self) -> bool {
        self.console_at != CLOSED
    }

    fn elapsed(&self, since: u64) -> u64 {
        self.now.saturating_sub(since)
    }

    fn toggle_ridge(&mut self) {
        if self.ridge_open() {
            self.ridge_at = CLOSED;
        } else {
            self.ridge_at = self.now;
            self.console_at = CLOSED;
        }
    }

    fn toggle_console(&mut self) {
        if self.console_open() {
            self.console_at = CLOSED;
        } else {
            self.console_at = self.now;
            self.ridge_at = CLOSED;
        }
    }
}

extern "C" fn rust_main() -> ! {
    let Some(info) = sys::display_info() else {
        sys::exit(1);
    };

    let buffer_len = info.buffer_len();
    let Some(addr) = sys::mmap(buffer_len, true) else {
        sys::exit(1);
    };

    // Safety: `mmap` returned this exact address as a writable mapping of at
    // least `buffer_len` bytes in this process's own address space, and this
    // process is single threaded.
    let buf = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, buffer_len as usize) };
    let mut canvas = Canvas { buf, info };

    let width = info.width as i32;
    let height = info.height as i32;

    let mut shell = Shell {
        cursor_x: width / 2,
        cursor_y: height / 2,
        dark: true,
        ridge_at: CLOSED,
        console_at: CLOSED,
        toast_at: CLOSED,
        bands: console::defaults(),
        hovered_app: None,
        frame: 0,
        now: sys::uptime_ticks(),
    };

    let started_at = shell.now;

    loop {
        shell.now = sys::uptime_ticks();

        if let Some(code) = pump_input(&mut shell, width, height) {
            // One line of cost accounting on the way out, since a shell that
            // redraws every pixel every frame should have to say what that
            // costs rather than being taken on trust.
            report_cost(&shell, started_at, buffer_len);
            sys::munmap(addr, buffer_len);
            sys::exit(code);
        }

        render(&mut canvas, &shell, width, height);

        // Safety: the mapping is still live and exactly `buffer_len` bytes.
        unsafe {
            sys::display_present(addr, buffer_len);
        }
        shell.frame += 1;
        sys::yield_now();
    }
}

/// Frames drawn, ticks elapsed, and the buffer size behind both.
///
/// This shell owns one mapping and no background work: when it is not the
/// foreground process it is not scheduled and costs nothing at all.
fn report_cost(shell: &Shell, started_at: u64, buffer_len: u64) {
    let ticks = shell.now.saturating_sub(started_at).max(1);
    // Frames per hundred ticks, which at the 100 Hz timer is frames per
    // second with two decimal places and no floating point.
    let fps_x100 = shell.frame.saturating_mul(10_000) / ticks;

    let mut digits = [0u8; 20];
    hello_user::write(b"tuwaiq-shell: frames=");
    hello_user::write(hello_user::u64_to_decimal(shell.frame, &mut digits));
    hello_user::write(b" ticks=");
    hello_user::write(hello_user::u64_to_decimal(ticks, &mut digits));
    hello_user::write(b" fps_x100=");
    hello_user::write(hello_user::u64_to_decimal(fps_x100, &mut digits));
    hello_user::write(b" buffer_bytes=");
    hello_user::write(hello_user::u64_to_decimal(buffer_len, &mut digits));
    hello_user::write(b"\n");
}

fn pump_input(shell: &mut Shell, width: i32, height: i32) -> Option<i64> {
    let mut buf = [0u8; 8];
    while sys::input_poll(&mut buf) {
        match buf[0] {
            1 => match buf[1] {
                sys::KEY_ESCAPE => return Some(0),
                b'a' | b'A' => shell.toggle_ridge(),
                b's' | b'S' => shell.toggle_console(),
                b'n' | b'N' => shell.toast_at = shell.now,
                b'l' | b'L' => shell.dark = !shell.dark,
                _ => {}
            },
            2 => {
                let x = i16::from_le_bytes([buf[4], buf[5]]) as i32;
                let y = i16::from_le_bytes([buf[6], buf[7]]) as i32;
                shell.cursor_x = x.clamp(0, width - 1);
                shell.cursor_y = y.clamp(0, height - 1);
                shell.hovered_app = if shell.ridge_open() {
                    ridge::hit(shell.cursor_x, shell.cursor_y, ridge_top(shell))
                } else {
                    None
                };
            }
            3 => {
                if buf[1] == 0 && buf[2] != 0 {
                    on_click(shell, width);
                }
            }
            _ => {}
        }
    }
    None
}

fn ridge_top(shell: &Shell) -> i32 {
    if shell.ridge_open() {
        ridge::offset(shell.elapsed(shell.ridge_at))
    } else {
        -ridge::HEIGHT
    }
}

fn on_click(shell: &mut Shell, width: i32) {
    let (x, y) = (shell.cursor_x, shell.cursor_y);

    // The mark occupies the leading end of the plateau.
    if y < escarpment::PLATEAU_H && x < 130 {
        shell.toggle_ridge();
        return;
    }
    // The tray sits at the trailing end of the plain.
    if y < escarpment::PLAIN_H && x > width - 130 && x < width - 70 {
        shell.toggle_console();
        return;
    }
    if shell.console_open() {
        let left = width - console::WIDTH + console::offset(shell.elapsed(shell.console_at));
        if let Some(index) = console::hit(x, y, left, escarpment::PLAIN_H) {
            shell.bands[index].on = !shell.bands[index].on;
            return;
        }
    }
    if shell.ridge_open() {
        if ridge::hit(x, y, ridge_top(shell)).is_some() {
            // A real launch would spawn here. The prototype closes instead,
            // so the gesture is complete without claiming a running app.
            shell.ridge_at = CLOSED;
            shell.toast_at = shell.now;
            return;
        }
        if y > ridge::HEIGHT {
            shell.ridge_at = CLOSED;
        }
    }
}

fn render(canvas: &mut Canvas, shell: &Shell, width: i32, height: i32) {
    let theme = shell.theme();

    // Ground. Two faint horizontal seams at the same proportions the panel
    // uses, so the desktop reads as strata rather than a flat fill.
    let (vr, vg, vb) = theme.void;
    canvas.fill_rect(0, 0, width, height, vr, vg, vb);
    let (pr, pg, pb) = theme.plain;
    canvas.fill_rect(0, height * 62 / 100, width, 1, pr, pg, pb);
    canvas.fill_rect(0, height * 74 / 100, width, 1, pr, pg, pb);

    // A single window, so focus has something to mark.
    draw_window(canvas, 96, height * 42 / 100, 380, 150, theme);

    if shell.ridge_open() {
        ridge::draw(canvas, ridge_top(shell), shell.hovered_app, theme);
    }

    escarpment::draw_panel(canvas, width, theme);
    draw_panel_content(canvas, shell, width, theme);

    if shell.console_open() {
        let left = width - console::WIDTH + console::offset(shell.elapsed(shell.console_at));
        console::draw(canvas, left, escarpment::PLAIN_H, &shell.bands, theme);
    }

    if shell.toast_at != CLOSED && shell.elapsed(shell.toast_at) < TOAST_TICKS {
        draw_toast(canvas, width, theme);
    }

    canvas.draw_cursor(shell.cursor_x, shell.cursor_y);
}

fn draw_panel_content(canvas: &mut Canvas, shell: &Shell, width: i32, theme: &Theme) {
    let (ir, ig, ib) = theme.ink;
    let (dr, dg, db) = theme.dim;
    let (cr, cg, cb) = theme.copper;

    escarpment::draw_mark(canvas, 16, 9, theme.copper);
    canvas.draw_text(b"TUWAIQ", 44, 12, ir, ig, ib);

    // Running applications carry a copper cap. Copper means the system acted,
    // and running is one of the four things it is allowed to mean.
    canvas.fill_rect(132, 8, 62, 2, cr, cg, cb);
    canvas.draw_text(b"MONITOR", 132, 14, ir, ig, ib);
    canvas.draw_text(b"NOTES", 210, 14, dr, dg, db);

    // Tray and clock live on the plain.
    let tray_x = width - 122;
    canvas.draw_text(b"NET", tray_x, 10, dr, dg, db);
    let (tr, tg, tb) = if shell.console_open() {
        theme.copper
    } else {
        theme.dim
    };
    canvas.draw_text(b"SYS", tray_x + 34, 10, tr, tg, tb);

    let seconds = sys::uptime_ticks() / 100;
    let mut clock = [b'0'; 5];
    let minutes = (seconds / 60) % 60;
    let hours = (seconds / 3600) % 24;
    clock[0] = b'0' + (hours / 10) as u8;
    clock[1] = b'0' + (hours % 10) as u8;
    clock[2] = b':';
    clock[3] = b'0' + (minutes / 10) as u8;
    clock[4] = b'0' + (minutes % 10) as u8;
    canvas.draw_text(&clock, width - 48, 10, ir, ig, ib);
}

fn draw_window(canvas: &mut Canvas, x: i32, y: i32, w: i32, h: i32, theme: &Theme) {
    let (chr, chg, chb) = theme.chrome;
    canvas.fill_rect(x, y, w, h, chr, chg, chb);

    let (plr, plg, plb) = theme.plateau;
    canvas.fill_rect(x, y, w, 22, plr, plg, plb);

    // Focus is copper, and it is the only thing that says focus.
    let (cr, cg, cb) = theme.copper;
    canvas.fill_rect(x, y, w, 1, cr, cg, cb);
    canvas.fill_rect(x + 8, y + 8, 5, 5, cr, cg, cb);

    let (ir, ig, ib) = theme.ink;
    let (dr, dg, db) = theme.dim;
    canvas.draw_text(b"MONITOR", x + 20, y + 8, ir, ig, ib);
    canvas.draw_text(b"UPTIME   04:12:38", x + 14, y + 40, dr, dg, db);
    canvas.draw_text(b"TASKS    7", x + 14, y + 56, dr, dg, db);
    canvas.draw_text(b"FRAMES   1051", x + 14, y + 72, dr, dg, db);
    canvas.draw_text(b"HEAP     151224", x + 14, y + 88, dr, dg, db);
    canvas.draw_text(b"FAULTS   0", x + 14, y + 104, dr, dg, db);
}

/// A notification lands on the ledge below the plain rather than floating
/// over the work, and is marked by the same copper edge everything else uses.
fn draw_toast(canvas: &mut Canvas, width: i32, theme: &Theme) {
    let w = 290;
    let h = 46;
    let x = width - w - 18;
    let y = escarpment::PLAIN_H + 10;

    let (chr, chg, chb) = theme.chrome;
    canvas.fill_rect(x, y, w, h, chr, chg, chb);
    let (cr, cg, cb) = theme.copper;
    canvas.fill_rect(x, y, 2, h, cr, cg, cb);

    let (ir, ig, ib) = theme.ink;
    let (dr, dg, db) = theme.dim;
    canvas.draw_text(b"CHECKPOINT WRITTEN", x + 12, y + 12, ir, ig, ib);
    canvas.draw_text(b"TUWAIQFS GEN 1051  SLOT B", x + 12, y + 28, dr, dg, db);
}

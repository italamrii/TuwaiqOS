//! The launcher, which rises rather than floats.
//!
//! A conventional launcher fades in over your work. This one raises the
//! horizon: the plateau grows upward and the application grid is already
//! carved into it. One vertical motion, no scale and no opacity, because the
//! escarpment is a solid thing and solid things do not dissolve.
//!
//! The animation is driven by the frame loop rather than a timer, so it stays
//! correct when the scheduler is busy: the offset is a function of how many
//! frames have passed since the launcher was asked to open.

use crate::escarpment::sub_profile;
use crate::gfx::Canvas;
use crate::theme::Theme;

pub const WIDTH: i32 = 460;
pub const HEIGHT: i32 = 300;
/// Ticks the rise takes at the 100 Hz timer, so 340 ms of wall clock
/// regardless of how many frames the renderer manages in that window.
pub const RISE_TICKS: u64 = 34;

pub const APPS: [&[u8]; 8] = [
    b"TERMINAL", b"FILES", b"NOTES", b"EDITOR", b"MONITOR", b"TUWAIQ AI", b"STORAGE", b"SETTINGS",
];

const COLS: i32 = 4;
const CELL_W: i32 = 106;
const CELL_H: i32 = 34;
const GRID_X: i32 = 18;
const GRID_Y: i32 = 96;

/// Eased offset for a rise `elapsed` ticks along.
///
/// Quadratic ease-out: fast departure, slow arrival. A linear rise reads as a
/// window being dragged; this reads as mass settling.
///
/// Driven by the clock rather than by frame count. The renderer here writes
/// every pixel through a bounds-checked `put_pixel`, so frame times are long
/// and uneven; counting frames made this take over a second and vary with
/// whatever else was running.
pub fn offset(elapsed: u64) -> i32 {
    if elapsed >= RISE_TICKS {
        return 0;
    }
    let remaining = (RISE_TICKS - elapsed) as i32;
    let span = RISE_TICKS as i32;
    -(HEIGHT * remaining * remaining) / (span * span)
}

/// Which app cell contains a point, in shell coordinates.
pub fn hit(x: i32, y: i32, top: i32) -> Option<usize> {
    let gx = x - GRID_X;
    let gy = y - (top + GRID_Y);
    if gx < 0 || gy < 0 {
        return None;
    }
    let col = gx / CELL_W;
    let row = gy / CELL_H;
    if col >= COLS || gx % CELL_W > CELL_W - 6 {
        return None;
    }
    let index = (row * COLS + col) as usize;
    if index < APPS.len() {
        Some(index)
    } else {
        None
    }
}

pub fn draw(canvas: &mut Canvas, top: i32, hovered: Option<usize>, theme: &Theme) {
    let (pr, pg, pb) = theme.plateau;
    for x in 0..WIDTH {
        let h = sub_profile(x, WIDTH, HEIGHT);
        let y = top;
        if y + h > 0 {
            canvas.fill_rect(x, y, 1, h, pr, pg, pb);
        }
    }

    // The same lit edge the panel uses, so the two read as one landform.
    let (cr, cg, cb) = theme.copper;
    for x in 0..WIDTH {
        let h = sub_profile(x, WIDTH, HEIGHT);
        canvas.put_pixel(x, top + h - 1, cr, cg, cb);
    }

    let (fr, fg, fb) = theme.faint;
    canvas.draw_text(b"ALL APPLICATIONS", GRID_X, top + 56, fr, fg, fb);

    // The search seam: a single copper rule, opened rather than boxed.
    let (dr, dg, db) = theme.dim;
    canvas.draw_text(b"SEARCH THE SYSTEM", GRID_X + 14, top + 72, fr, fg, fb);
    canvas.fill_rect(GRID_X, top + 86, WIDTH - GRID_X * 2, 1, cr, cg, cb);
    canvas.fill_rect(GRID_X, top + 73, 6, 6, cr, cg, cb);
    let _ = (dr, dg, db);

    for (index, name) in APPS.iter().enumerate() {
        let col = index as i32 % COLS;
        let row = index as i32 / COLS;
        let x = GRID_X + col * CELL_W;
        let y = top + GRID_Y + row * CELL_H;
        let active = hovered == Some(index);
        if active {
            let (wr, wg, wb) = theme.copper_wash;
            canvas.fill_rect(x, y, CELL_W - 6, CELL_H - 6, wr, wg, wb);
            canvas.fill_rect(x, y, 2, CELL_H - 6, cr, cg, cb);
        }
        let (tr, tg, tb) = if active { theme.ink } else { theme.dim };
        canvas.draw_text(name, x + 10, y + 11, tr, tg, tb);
    }
}

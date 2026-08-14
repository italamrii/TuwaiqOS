//! Quick settings as strata.
//!
//! Every desktop draws toggles as round pills because every desktop copies
//! the same phone. These are horizontal bands that fill copper from the
//! leading edge, so a panel of settings reads as rock layers and a toggle
//! reads as a layer being lit rather than a switch being flipped.
//!
//! Filling from the leading edge rather than the left means the fill still
//! travels toward the reader once the shell is mirrored for Arabic.

use crate::escarpment::sub_profile;
use crate::gfx::Canvas;
use crate::theme::Theme;

pub const WIDTH: i32 = 300;
pub const HEIGHT: i32 = 232;
/// Ticks the slide takes at the 100 Hz timer, so 240 ms of wall clock
pub const SLIDE_TICKS: u64 = 24;

const BAND_H: i32 = 30;
const BAND_GAP: i32 = 4;
const PAD: i32 = 14;

pub struct Band {
    pub label: &'static [u8],
    pub on_value: &'static [u8],
    pub off_value: &'static [u8],
    pub on: bool,
}

pub const COUNT: usize = 5;

pub fn defaults() -> [Band; COUNT] {
    [
        Band { label: b"NETWORK", on_value: b"LOOPBACK", off_value: b"DOWN", on: true },
        Band { label: b"SOUND", on_value: b"ON", off_value: b"OFF", on: false },
        Band { label: b"NIGHT", on_value: b"ON", off_value: b"OFF", on: true },
        Band { label: b"CAPTURE", on_value: b"REC", off_value: b"IDLE", on: false },
        Band { label: b"BRIGHTNESS", on_value: b"72%", off_value: b"DIM", on: true },
    ]
}

/// Eased horizontal offset for a slide `elapsed` ticks along. Clock driven
/// for the same reason the launcher is -- see `ridge::offset`.
pub fn offset(elapsed: u64) -> i32 {
    if elapsed >= SLIDE_TICKS {
        return 0;
    }
    let remaining = (SLIDE_TICKS - elapsed) as i32;
    let span = SLIDE_TICKS as i32;
    (WIDTH * remaining * remaining) / (span * span)
}

pub fn hit(x: i32, y: i32, left: i32, top: i32) -> Option<usize> {
    let bx = x - left;
    let by = y - (top + PAD);
    if bx < PAD || bx > WIDTH - PAD || by < 0 {
        return None;
    }
    let index = (by / (BAND_H + BAND_GAP)) as usize;
    if by % (BAND_H + BAND_GAP) > BAND_H {
        return None;
    }
    if index < COUNT {
        Some(index)
    } else {
        None
    }
}

pub fn draw(canvas: &mut Canvas, left: i32, top: i32, bands: &[Band; COUNT], theme: &Theme) {
    let (pr, pg, pb) = theme.plateau;
    // Mirrored break: the console hangs off the trailing edge, so its own
    // step points the other way and the two silhouettes bracket the screen.
    for x in 0..WIDTH {
        let h = sub_profile(WIDTH - 1 - x, WIDTH, HEIGHT);
        canvas.fill_rect(left + x, top, 1, h, pr, pg, pb);
    }
    let (lr, lg, lb) = theme.line;
    canvas.fill_rect(left, top, 1, HEIGHT, lr, lg, lb);

    let (cr, cg, cb) = theme.copper;
    for x in 0..WIDTH {
        let h = sub_profile(WIDTH - 1 - x, WIDTH, HEIGHT);
        canvas.put_pixel(left + x, top + h - 1, cr, cg, cb);
    }

    for (index, band) in bands.iter().enumerate() {
        let y = top + PAD + index as i32 * (BAND_H + BAND_GAP);
        let x = left + PAD;
        let w = WIDTH - PAD * 2;

        let (vr, vg, vb) = theme.void;
        canvas.fill_rect(x, y, w, BAND_H, vr, vg, vb);

        if band.on {
            let (wr, wg, wb) = theme.copper_wash;
            canvas.fill_rect(x, y, w, BAND_H, wr, wg, wb);
            canvas.fill_rect(x, y, 2, BAND_H, cr, cg, cb);
        }

        let (tr, tg, tb) = if band.on { theme.ink } else { theme.dim };
        canvas.draw_text(band.label, x + 12, y + 11, tr, tg, tb);

        let value = if band.on { band.on_value } else { band.off_value };
        let (fr, fg, fb) = theme.faint;
        let vx = x + w - 10 - value.len() as i32 * 8;
        canvas.draw_text(value, vx, y + 11, fr, fg, fb);
    }
}

//! The panel, which is a silhouette rather than a bar.
//!
//! Tuwaiq is a plateau, one sharp break, and a lower plain. That profile is
//! the shape of the panel itself: the system zone sits on a deeper plateau,
//! the task zone on a lower plain, and a single angled break joins them.
//!
//! No other desktop has a non-rectangular panel. That one decision is what
//! makes the system identifiable before a word is read, and every other
//! surface in this shell repeats the same break at a smaller scale.
//!
//! Depth is a lit edge and never a shadow: one copper hairline runs along the
//! profile, the way first light catches a cliff rim. It stays sharp at any
//! resolution and costs one pixel per column to draw.

use crate::gfx::Canvas;
use crate::theme::{Rgb, Theme};

/// Height of the plateau, which is also the panel's tallest point
pub const PLATEAU_H: i32 = 44;
/// Height of the plain
pub const PLAIN_H: i32 = 28;
/// Where the plateau ends and the break begins
pub const BREAK_X: i32 = 380;
/// How wide the break is. A vertical drop would read as a mistake; this
/// reads as terrain.
pub const SLOPE_W: i32 = 26;

/// The panel's lower edge at column `x`.
pub fn profile(x: i32) -> i32 {
    if x < BREAK_X {
        PLATEAU_H
    } else if x < BREAK_X + SLOPE_W {
        let t = x - BREAK_X;
        PLATEAU_H - ((PLATEAU_H - PLAIN_H) * t) / SLOPE_W
    } else {
        PLAIN_H
    }
}

/// Fill a region whose lower edge follows `profile`, then light that edge.
pub fn draw_panel(canvas: &mut Canvas, width: i32, theme: &Theme) {
    for x in 0..width {
        let h = profile(x);
        let (r, g, b) = if x < BREAK_X + SLOPE_W {
            theme.plateau
        } else {
            theme.plain
        };
        canvas.fill_rect(x, 0, 1, h, r, g, b);
    }
    edge_light(canvas, width, theme.copper);
}

/// One copper hairline along the profile. Drawn two pixels thick on the
/// plateau and one on the plain, so the deeper mass reads as closer to the
/// light rather than merely taller.
pub fn edge_light(canvas: &mut Canvas, width: i32, copper: Rgb) {
    let (r, g, b) = copper;
    for x in 0..width {
        let h = profile(x);
        canvas.put_pixel(x, h - 1, r, g, b);
        if x < BREAK_X {
            canvas.put_pixel(x, h - 2, r, g, b);
        }
    }
}

/// The mark: the escarpment reduced to three strokes. Not a logo lockup and
/// deliberately not a falcon -- it is the same profile the panel draws, small
/// enough to sit in a 44 pixel band.
pub fn draw_mark(canvas: &mut Canvas, x: i32, y: i32, copper: Rgb) {
    let (r, g, b) = copper;
    // low plain
    canvas.fill_rect(x, y + 9, 7, 2, r, g, b);
    // the break
    for i in 0..5 {
        canvas.fill_rect(x + 7 + i, y + 9 - i * 2, 2, 2, r, g, b);
    }
    // high plateau
    canvas.fill_rect(x + 12, y, 8, 2, r, g, b);
}

/// A smaller version of the same break, for panels that rise out of the
/// plateau. Returns the lower edge at `x` for a surface of height `h`.
pub fn sub_profile(x: i32, w: i32, h: i32) -> i32 {
    let break_at = w - w / 6;
    let slope = w / 12;
    if x < break_at {
        h
    } else if x < break_at + slope && slope > 0 {
        h - ((h / 5) * (x - break_at)) / slope
    } else {
        h - h / 5
    }
}

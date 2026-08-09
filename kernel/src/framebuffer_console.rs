//! Framebuffer text console.
//!
//! Renders scalable terminal-style text on the bootloader framebuffer.
//! An 8x8 VGA font is scaled up (2x wide, 3x tall) so text is readable at
//! 1280x720, similar to a classic Windows CMD window.

use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};

use crate::font8x8;

const FONT_WIDTH: usize = 8;
const FONT_HEIGHT: usize = 8;
const SCALE_X: usize = 2;
const SCALE_Y: usize = 3;
const CHAR_WIDTH: usize = FONT_WIDTH * SCALE_X + 2; // 18
const CHAR_HEIGHT: usize = FONT_HEIGHT * SCALE_Y + 6; // 30

const FG_R: u8 = 0xFF;
const FG_G: u8 = 0xFF;
const FG_B: u8 = 0xFF;

const BG_R: u8 = 0x00;
const BG_G: u8 = 0x00;
const BG_B: u8 = 0x00;

static mut FB_BUFFER: Option<*mut u8> = None;
static mut FB_INFO: Option<FrameBufferInfo> = None;
static mut CURSOR_X: usize = 0;
static mut CURSOR_Y: usize = 0;

/// Whether the framebuffer console is the active console. Fault/panic paths
/// use this to decide which console is *safe* to write to: on a graphical
/// boot the legacy VGA text buffer (0xB8000) is typically not mapped at
/// all, so blindly writing to both consoles (as v0.5's panic handler did)
/// can turn a fault handler into a fault storm against unmapped memory.
pub fn is_active() -> bool {
    // Safety: reads through a raw pointer rather than `&FB_INFO`, matching
    // the pattern the rest of this module already uses to avoid creating a
    // live shared reference to a `static mut`.
    unsafe { (*core::ptr::addr_of!(FB_INFO)).is_some() }
}

/// Attach the console to the bootloader-provided framebuffer.
pub fn try_init(framebuffer: &mut FrameBuffer) -> Result<(), &'static str> {
    let info = framebuffer.info();
    let minimum_bpp = match info.pixel_format {
        PixelFormat::Rgb | PixelFormat::Bgr => 3,
        PixelFormat::U8 => 1,
        PixelFormat::Unknown { .. } => return Err("unsupported framebuffer pixel format"),
        _ => return Err("unsupported framebuffer pixel format"),
    };
    validate_geometry(
        info.width,
        info.height,
        info.stride,
        info.bytes_per_pixel,
        minimum_bpp,
        info.byte_len,
        framebuffer.buffer().len(),
    )?;
    unsafe {
        FB_BUFFER = Some(framebuffer.buffer_mut().as_mut_ptr());
        FB_INFO = Some(info);
        CURSOR_X = 0;
        CURSOR_Y = 0;
    }
    Ok(())
}

fn validate_geometry(
    width: usize,
    height: usize,
    stride: usize,
    bytes_per_pixel: usize,
    minimum_bpp: usize,
    advertised_len: usize,
    actual_len: usize,
) -> Result<(), &'static str> {
    if width == 0 || height == 0 || stride < width {
        return Err("invalid framebuffer dimensions or stride");
    }
    if bytes_per_pixel < minimum_bpp || bytes_per_pixel > 8 {
        return Err("invalid framebuffer bytes-per-pixel");
    }
    let required = stride
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .ok_or("framebuffer geometry overflow")?;
    if required > advertised_len || actual_len < required {
        return Err("framebuffer buffer is shorter than its geometry");
    }
    Ok(())
}

pub fn validation_self_test() -> bool {
    validate_geometry(0, 720, 1280, 4, 3, 4_000_000, 4_000_000).is_err()
        && validate_geometry(1280, 720, 1279, 4, 3, 4_000_000, 4_000_000).is_err()
        && validate_geometry(usize::MAX, 2, usize::MAX, 8, 3, usize::MAX, usize::MAX).is_err()
        && validate_geometry(1280, 720, 1280, 4, 3, 512, 512).is_err()
        && validate_geometry(1280, 720, 1280, 4, 3, 3_686_400, 3_686_400).is_ok()
}

/// Fill the entire framebuffer with the background color.
pub fn clear_screen() {
    with_console(|info| {
        let buffer = framebuffer_slice(info);
        buffer.fill(0);
        unsafe {
            CURSOR_X = 0;
            CURSOR_Y = 0;
        }
    });
}

/// Print a line of text, then advance to the next row.
pub fn println(text: &str) {
    print(text);
    newline();
}

/// Print text without a trailing newline.
pub fn print(text: &str) {
    for byte in text.bytes() {
        write_char(byte);
    }
}

/// Print the minimal shell prompt: `> `
pub fn print_prompt() {
    print("> ");
}

/// Erase the character left of the cursor (used for backspace).
pub fn backspace() {
    unsafe {
        if CURSOR_X == 0 {
            return;
        }
        CURSOR_X -= 1;
    }

    with_console(|info| unsafe {
        draw_glyph(info, b' ', CURSOR_X, CURSOR_Y);
    });
}

fn newline() {
    unsafe {
        CURSOR_X = 0;
        CURSOR_Y += 1;

        if let Some(info) = framebuffer_info() {
            let max_rows = max_rows(info);
            if CURSOR_Y >= max_rows {
                scroll_up();
            }
        }
    }
}

fn write_char(byte: u8) {
    let character = match byte {
        0x20..=0x7e => byte,
        b'\n' => {
            newline();
            return;
        }
        _ => b'?',
    };

    with_console(|info| {
        let max_cols = max_cols(info);
        let max_rows = max_rows(info);

        unsafe {
            if CURSOR_X >= max_cols {
                CURSOR_X = 0;
                CURSOR_Y += 1;
            }

            if CURSOR_Y >= max_rows {
                scroll_up();
            }

            draw_glyph(info, character, CURSOR_X, CURSOR_Y);
            CURSOR_X += 1;
        }
    });
}

fn max_cols(info: FrameBufferInfo) -> usize {
    info.width / CHAR_WIDTH
}

fn max_rows(info: FrameBufferInfo) -> usize {
    info.height / CHAR_HEIGHT
}

fn scroll_up() {
    with_console(|info| {
        let row_bytes = info.stride * info.bytes_per_pixel;
        let buffer = framebuffer_slice(info);
        let scroll = CHAR_HEIGHT;

        for y in scroll..info.height {
            let src = y * row_bytes;
            let dst = (y - scroll) * row_bytes;
            buffer.copy_within(src..src + row_bytes, dst);
        }

        let clear_start = (info.height - scroll) * row_bytes;
        buffer[clear_start..info.height * row_bytes].fill(0);

        unsafe {
            if CURSOR_Y > 0 {
                CURSOR_Y -= 1;
            }
            CURSOR_X = 0;
        }
    });
}

fn draw_glyph(info: FrameBufferInfo, ch: u8, col: usize, row: usize) {
    let glyph = font8x8::glyph_rows(ch);
    let base_x = col * CHAR_WIDTH;
    let base_y = row * CHAR_HEIGHT;

    fill_rect(
        info,
        base_x,
        base_y,
        CHAR_WIDTH,
        CHAR_HEIGHT,
        BG_R,
        BG_G,
        BG_B,
    );

    for (font_y, row_bits) in glyph.iter().enumerate().take(FONT_HEIGHT) {
        for font_x in 0..FONT_WIDTH {
            if !font8x8::pixel_is_set(*row_bits, font_x) {
                continue;
            }

            let pixel_x = base_x + font_x * SCALE_X;
            let pixel_y = base_y + font_y * SCALE_Y;
            fill_rect(info, pixel_x, pixel_y, SCALE_X, SCALE_Y, FG_R, FG_G, FG_B);
        }
    }
}

fn fill_rect(
    info: FrameBufferInfo,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    r: u8,
    g: u8,
    b: u8,
) {
    for dy in 0..height {
        for dx in 0..width {
            write_pixel(info, x + dx, y + dy, r, g, b);
        }
    }
}

fn write_pixel(info: FrameBufferInfo, x: usize, y: usize, r: u8, g: u8, b: u8) {
    if x >= info.width || y >= info.height {
        return;
    }

    let pixel_index = y * info.stride + x;
    let byte_index = pixel_index * info.bytes_per_pixel;
    let buffer = framebuffer_slice(info);

    if byte_index + info.bytes_per_pixel > buffer.len() {
        return;
    }

    match info.pixel_format {
        PixelFormat::Bgr => {
            buffer[byte_index] = b;
            buffer[byte_index + 1] = g;
            buffer[byte_index + 2] = r;
            if info.bytes_per_pixel >= 4 {
                buffer[byte_index + 3] = 0;
            }
        }
        PixelFormat::Rgb => {
            buffer[byte_index] = r;
            buffer[byte_index + 1] = g;
            buffer[byte_index + 2] = b;
            if info.bytes_per_pixel >= 4 {
                buffer[byte_index + 3] = 0;
            }
        }
        PixelFormat::U8 => {
            buffer[byte_index] = ((r as u16 + g as u16 + b as u16) / 3) as u8;
        }
        PixelFormat::Unknown { .. } => {
            if info.bytes_per_pixel >= 3 {
                buffer[byte_index] = r;
                buffer[byte_index + 1] = g;
                buffer[byte_index + 2] = b;
                if info.bytes_per_pixel >= 4 {
                    buffer[byte_index + 3] = 0;
                }
            } else if info.bytes_per_pixel >= 1 {
                buffer[byte_index] = ((r as u16 + g as u16 + b as u16) / 3) as u8;
            }
        }
        _ => {
            if info.bytes_per_pixel >= 3 {
                buffer[byte_index] = r;
                buffer[byte_index + 1] = g;
                buffer[byte_index + 2] = b;
                if info.bytes_per_pixel >= 4 {
                    buffer[byte_index + 3] = 0;
                }
            } else if info.bytes_per_pixel >= 1 {
                buffer[byte_index] = ((r as u16 + g as u16 + b as u16) / 3) as u8;
            }
        }
    }

    unsafe {
        core::ptr::read_volatile(buffer.as_ptr().add(byte_index));
    }
}

fn framebuffer_slice(info: FrameBufferInfo) -> &'static mut [u8] {
    unsafe {
        core::slice::from_raw_parts_mut(
            FB_BUFFER.expect("framebuffer not initialized"),
            info.byte_len,
        )
    }
}

fn framebuffer_info() -> Option<FrameBufferInfo> {
    unsafe { FB_INFO }
}

/// Public accessor for `display.rs` (Phase 5): the same info the text
/// console already uses internally for its own glyph placement, exposed
/// read-only so `display::info()` doesn't need to duplicate framebuffer
/// bookkeeping the console already owns.
pub fn raw_info() -> Option<FrameBufferInfo> {
    framebuffer_info()
}

/// Public accessor for `display.rs` (Phase 5): direct, whole-buffer access
/// to the real framebuffer, bypassing the glyph-drawing text-console path
/// entirely -- what `display::present` copies a validated userspace
/// pixel buffer into. Once a desktop process starts presenting frames, its
/// content simply overwrites whatever the text console last drew, the same
/// way a real OS's GUI takes over the screen from a boot console; nothing
/// about the console's own `CURSOR_X`/`CURSOR_Y` state needs to change for
/// that -- it's simply stale until `clear_screen()` resets it, which the
/// shell does after a desktop process exits.
pub fn raw_buffer_mut() -> Option<&'static mut [u8]> {
    framebuffer_info().map(framebuffer_slice)
}

fn with_console<F>(f: F)
where
    F: FnOnce(FrameBufferInfo),
{
    if let Some(info) = framebuffer_info() {
        f(info);
    }
}

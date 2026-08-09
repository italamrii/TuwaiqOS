//! VGA text-mode output.
//!
//! Writes characters directly to the PC text buffer at physical address `0xB8000`.
//! Each screen cell is 2 bytes: ASCII character + color attribute.

const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;

/// Default color: light gray on black.
const DEFAULT_COLOR: u8 = 0x07;

pub fn try_init() -> Result<(), &'static str> {
    let address = x86_64::VirtAddr::new(VGA_BUFFER as u64);
    if crate::paging::translate_kernel_address(address).is_none() {
        crate::paging::map_mmio_range(VGA_BUFFER as u64, 4096, VGA_BUFFER as u64)?;
    }
    Ok(())
}

static mut ROW: usize = 0;
static mut COL: usize = 0;

/// Clear the entire 80x25 text screen.
pub fn clear_screen() {
    unsafe {
        for row in 0..VGA_HEIGHT {
            for col in 0..VGA_WIDTH {
                write_cell(row, col, b' ', DEFAULT_COLOR);
            }
        }
        ROW = 0;
        COL = 0;
    }
}

/// Print a line of text, then move to the next row.
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
        if COL == 0 {
            return;
        }
        COL -= 1;
        write_cell(ROW, COL, b' ', DEFAULT_COLOR);
    }
}

fn newline() {
    unsafe {
        ROW += 1;
        COL = 0;

        if ROW >= VGA_HEIGHT {
            scroll_up();
        }
    }
}

fn write_char(byte: u8) {
    let character = match byte {
        // Printable ASCII
        0x20..=0x7e => byte,
        // Treat newline as handled by caller; other bytes become a block.
        b'\n' => {
            newline();
            return;
        }
        _ => 0xfe,
    };

    unsafe {
        if COL >= VGA_WIDTH {
            newline();
        }

        write_cell(ROW, COL, character, DEFAULT_COLOR);
        COL += 1;
    }
}

fn scroll_up() {
    unsafe {
        for row in 1..VGA_HEIGHT {
            for col in 0..VGA_WIDTH {
                let src = cell_offset(row, col);
                let dst = cell_offset(row - 1, col);
                let color = *VGA_BUFFER.add(src + 1);
                let ch = *VGA_BUFFER.add(src);
                *VGA_BUFFER.add(dst) = ch;
                *VGA_BUFFER.add(dst + 1) = color;
            }
        }

        let last_row = VGA_HEIGHT - 1;
        for col in 0..VGA_WIDTH {
            write_cell(last_row, col, b' ', DEFAULT_COLOR);
        }

        ROW = VGA_HEIGHT - 1;
    }
}

fn write_cell(row: usize, col: usize, character: u8, color: u8) {
    let offset = cell_offset(row, col);
    unsafe {
        *VGA_BUFFER.add(offset) = character;
        *VGA_BUFFER.add(offset + 1) = color;
    }
}

fn cell_offset(row: usize, col: usize) -> usize {
    (row * VGA_WIDTH + col) * 2
}

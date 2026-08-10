//! TuwaiqOS kernel entry point.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

#[macro_use]
mod serial;

mod ai_bridge;
mod allocator;
mod banner;
mod apps;
mod ata;
mod display;
mod elf;
mod fat32;
mod font8x8;
mod logo;
mod framebuffer_console;
mod fs;
mod gdt;
mod input;
mod interrupts;
mod keyboard;
mod loader;
mod memory;
mod mouse;
mod net;
mod paging;
mod programs;
mod reboot;
mod shell;
mod syscall;
mod task;
mod tuwaiqfs;
mod usermode;
mod vfs;
mod vga_buffer;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{entry_point, BootInfo};

use shell::ConsoleMode;

/// The default config leaves `physical_memory_offset` unset (`None`), which
/// means the bootloader's own page tables -- and therefore any physical
/// address at all -- are inaccessible to the kernel. Phase 2's frame
/// allocator and page-table walking both need to translate physical
/// addresses to something dereferenceable, so the whole of physical memory
/// is mapped at a bootloader-chosen (`Dynamic`) virtual offset instead.
static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial_println!("TuwaiqOS v0.5 kernel_main: booting");

    // Must run before any page table exists: see `paging::enable_nx`.
    paging::enable_nx();

    // Bring the display up before anything slow. `framebuffer_console` needs
    // no heap, no interrupts and no locks -- it is plain MMIO into a region
    // the bootloader mapped before entry -- so it is valid this early, and
    // doing it here means the splash covers the ATA probe and driver setup
    // instead of appearing after them. The borrow ends with this block,
    // leaving `boot_info` free for the `'static` reborrow `init_heap` needs.
    if let Some(framebuffer) = boot_info.framebuffer.as_mut() {
        framebuffer_console::init(framebuffer);
        framebuffer_console::clear_screen();
        draw_splash();
    }


    memory::init_heap(
        boot_info.physical_memory_offset.into_option(),
        &boot_info.memory_regions,
    );
    serial_println!(
        "heap: {} bytes online (paged, not a static array)",
        memory::HEAP_SIZE
    );

    // Must come after the heap (the keyboard event queue allocates) and
    // before anything relies on real interrupts, real ticks, or
    // interrupt-driven keyboard input.
    interrupts::init();

    // Programs the PS/2 auxiliary device, then unmasks its IRQ line only
    // once that's done (see `interrupts::enable_mouse`'s docs for why that
    // ordering matters).
    // IRQ1 and PS/2 controller replies share port 0x60. Keep the short,
    // bounded auxiliary-device transaction exclusive so the keyboard ISR
    // cannot consume a configuration byte or mouse ACK mid-initialization.
    let mouse_ready = x86_64::instructions::interrupts::without_interrupts(mouse::init);
    if mouse_ready {
        interrupts::enable_mouse();
    } else {
        serial_println!("mouse: IRQ12 remains masked; boot continues without mouse input");
    }

    ata::init();
    vfs::init();
    task::init();
    net::init();

    if framebuffer_console::is_active() {
        if font8x8::DIAGNOSTIC_AT_BOOT {
            for line in font8x8::DIAGNOSTIC_LINES {
                framebuffer_console::println(line);
            }
            framebuffer_console::println("");
        }

        end_splash();
        framebuffer_console::println("TuwaiqOS v0.5");
        framebuffer_console::println("AI-Native Experimental Operating System");
        framebuffer_console::println("");

        shell::run(boot_info, ConsoleMode::Framebuffer);
    } else {
        vga_buffer::clear_screen();
        vga_buffer::println("TuwaiqOS v0.5");
        vga_buffer::println("AI-Native Experimental Operating System");
        vga_buffer::println("");

        shell::run(boot_info, ConsoleMode::Vga);
    }
}

/// Tuwaiq green. Flag green (#006C35) measures 3.19:1 against black, below
/// the 4.5:1 needed for legible marks; this keeps the hue and lifts the
/// luminance to 5.46:1.
const ACCENT: (u8, u8, u8) = (0x00, 0x96, 0x4A);

/// Draw the boot splash: the mark, a rule, and the tagline, centred on an
/// otherwise empty screen. Held for the whole of boot and cleared by
/// `end_splash`, so driver bring-up never scrolls past the user.
///
/// Laid out absolutely rather than through the text cursor, so nothing the
/// console prints later can land on top of it.
fn draw_splash() {
    let Some((width, height)) = framebuffer_console::dimensions() else {
        return;
    };

    const RULE_H: usize = 2;
    const GAP_RULE: usize = 30;
    const GAP_TAG: usize = 22;

    let tagline = &banner::TAGLINE;
    let block = logo::SIZE + GAP_RULE + RULE_H + GAP_TAG + tagline.height;
    // Optically centred: the mark is top-heavy, so a geometric centre reads
    // as sitting low. Lift it by a sixteenth of the screen.
    let top = height.saturating_sub(block) / 2;
    let top = top.saturating_sub(height / 16);

    let logo_x = width.saturating_sub(logo::SIZE) / 2;
    framebuffer_console::blit_at(&logo::STRUCTURE, logo_x, top, 0xFF, 0xFF, 0xFF);
    // The accent has exactly one job: the monogram, the only element that is
    // purely identity rather than information. Rule and type stay white.
    framebuffer_console::blit_at(&logo::MONOGRAM, logo_x, top, ACCENT.0, ACCENT.1, ACCENT.2);

    // A rule the width of the tagline ties the two blocks into one column.
    // White, matching the type -- structure and text share one voice.
    let rule_y = top + logo::SIZE + GAP_RULE;
    let rule_x = width.saturating_sub(tagline.width) / 2;
    framebuffer_console::fill(rule_x, rule_y, tagline.width, RULE_H, 0xFF, 0xFF, 0xFF);

    framebuffer_console::blit_at(tagline, rule_x, rule_y + RULE_H + GAP_TAG, 0xFF, 0xFF, 0xFF);
}

/// Minimum time the splash stays up, in PIT ticks (100 Hz -- see
/// `interrupts::init`). Boot on a warm QEMU image finishes in well under a
/// second, which would flash the identity past too fast to read; on real
/// hardware the drivers themselves consume this and the wait is a no-op.
const SPLASH_MIN_TICKS: u64 = 180;

/// Take the splash down and hand the screen to the console.
///
/// Called once boot is complete, so the splash covered every slow step
/// rather than appearing after them. Holds until `SPLASH_MIN_TICKS` have
/// elapsed, halting rather than spinning so the CPU is idle while it waits.
fn end_splash() {
    while interrupts::ticks() < SPLASH_MIN_TICKS {
        interrupts::halt();
    }
    framebuffer_console::clear_screen();
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Serial gets the full message + source location, unconditionally --
    // it's port I/O, not memory-mapped, so it's safe from any fault
    // context. The framebuffer console is only touched if confirmed
    // initialized: this boot configuration's page tables do not map the
    // legacy VGA text buffer (0xB8000) at all, so the old unconditional
    // vga_buffer write here would turn a panic into a recursive
    // page-fault storm instead of a clean halt (see interrupts::report_fault
    // for the full story).
    serial_println!("\n=== KERNEL PANIC ===\n{}\n=====================", info);
    if framebuffer_console::is_active() {
        framebuffer_console::println("");
        framebuffer_console::println("KERNEL PANIC");
    }
    loop {
        x86_64::instructions::hlt();
    }
}

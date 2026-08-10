//! TuwaiqOS kernel entry point.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

#[macro_use]
mod serial;

mod ai_bridge;
mod allocator;
mod apps;
mod ata;
mod display;
mod elf;
mod fat32;
mod font8x8;
mod framebuffer_console;
mod fs;
mod gdt;
mod input;
mod interrupts;
mod ipc;
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

    if let Some(framebuffer) = boot_info.framebuffer.as_mut() {
        framebuffer_console::init(framebuffer);
        framebuffer_console::clear_screen();

        if font8x8::DIAGNOSTIC_AT_BOOT {
            for line in font8x8::DIAGNOSTIC_LINES {
                framebuffer_console::println(line);
            }
            framebuffer_console::println("");
        }

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

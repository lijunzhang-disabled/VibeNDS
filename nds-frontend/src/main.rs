//! NDS emulator frontend (Phase 1 skeleton).
//!
//! Opens a single SDL2 window sized for two stacked 256x192 screens with
//! a 90 px gap, clears to black, and steps the core. Rendering of the
//! actual framebuffers is deferred to Phase 3.

mod video;

use clap::Parser;
use nds_core::Nds;
use sdl2::event::Event;
use sdl2::keyboard::{Keycode, Scancode};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "nds-emu", about = "NDS Emulator (work in progress)")]
struct Args {
    /// Path to the .nds ROM (optional in Phase 1).
    #[arg(long)]
    rom: Option<PathBuf>,

    /// Path to ARM9 BIOS dump (optional — defaults to a 0xFF-filled stub).
    #[arg(long)]
    bios_arm9: Option<PathBuf>,

    /// Path to ARM7 BIOS dump.
    #[arg(long)]
    bios_arm7: Option<PathBuf>,

    /// Path to firmware dump (Phase 5).
    #[arg(long)]
    firmware: Option<PathBuf>,

    /// Window scale factor.
    #[arg(short, long, default_value_t = 2)]
    scale: u32,
}

fn read_optional(p: Option<PathBuf>) -> Option<Vec<u8>> {
    let p = p?;
    match fs::read(&p) {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!("warning: could not read {}: {}", p.display(), e);
            None
        }
    }
}

fn main() {
    env_logger::init();
    let args = Args::parse();

    let bios9 = read_optional(args.bios_arm9);
    let bios7 = read_optional(args.bios_arm7);

    let mut nds = Nds::new(bios9, bios7);

    if let Some(rom) = &args.rom {
        match fs::read(rom) {
            Ok(bytes) => {
                eprintln!("Loaded ROM: {} ({} bytes)", rom.display(), bytes.len());
                match nds.load_cart_direct_boot(bytes) {
                    Ok(()) => {
                        if let Some(h) = nds.cart.header() {
                            eprintln!(
                                "Direct boot: title={:?} gamecode={} (CRC {})",
                                h.title,
                                h.gamecode_str(),
                                if h.header_crc_valid() { "valid" } else { "INVALID" }
                            );
                            eprintln!(
                                "  ARM9: load=0x{:08X} entry=0x{:08X} size=0x{:X}",
                                h.arm9_load, h.arm9_entry, h.arm9_size
                            );
                            eprintln!(
                                "  ARM7: load=0x{:08X} entry=0x{:08X} size=0x{:X}",
                                h.arm7_load, h.arm7_entry, h.arm7_size
                            );
                        }
                    }
                    Err(e) => eprintln!("direct boot failed: {}", e),
                }
            }
            Err(e) => eprintln!("warning: could not read ROM: {}", e),
        }
    } else {
        eprintln!("no ROM specified — running an empty system");
    }

    let sdl = sdl2::init().expect("failed to init SDL2");
    let mut display = video::DualScreen::new(&sdl, args.scale);
    let mut events = sdl.event_pump().expect("event pump");

    let frame_target = Duration::from_micros(16_715); // ~59.83 Hz
    'main_loop: loop {
        let frame_start = Instant::now();

        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => break 'main_loop,
                Event::KeyDown { keycode: Some(Keycode::Escape), .. } => break 'main_loop,
                _ => {}
            }
        }

        // Sample keyboard, translate to KEYINPUT bits.
        let kb = events.keyboard_state();
        let pressed = |sc: Scancode| kb.is_scancode_pressed(sc);
        let mut keys = 0u16;
        // KEYINPUT is active-low: bit = 1 means released.
        if !pressed(Scancode::Z)      { keys |= 1 << 0; } else {}        // A
        if !pressed(Scancode::X)      { keys |= 1 << 1; }                // B
        if !pressed(Scancode::RShift) { keys |= 1 << 2; }                // Select
        if !pressed(Scancode::Return) { keys |= 1 << 3; }                // Start
        if !pressed(Scancode::Right)  { keys |= 1 << 4; }
        if !pressed(Scancode::Left)   { keys |= 1 << 5; }
        if !pressed(Scancode::Up)     { keys |= 1 << 6; }
        if !pressed(Scancode::Down)   { keys |= 1 << 7; }
        if !pressed(Scancode::S)      { keys |= 1 << 8; }                // R shoulder
        if !pressed(Scancode::A)      { keys |= 1 << 9; }                // L shoulder
        nds.set_keys(keys);

        nds.run_frame();
        display.present(&nds.framebuffer_top, &nds.framebuffer_bot);

        let elapsed = frame_start.elapsed();
        if elapsed < frame_target {
            std::thread::sleep(frame_target - elapsed);
        }
    }

    eprintln!("nds-frontend: exiting");
}

//! NDS emulator frontend (Phase 1 skeleton).
//!
//! Opens a single SDL2 window sized for two stacked 256x192 screens with
//! a 90 px gap, clears to black, and steps the core. Rendering of the
//! actual framebuffers is deferred to Phase 3.

mod video;

use clap::Parser;
use nds_core::{Nds, SCREEN_HEIGHT, SCREEN_WIDTH};
use nds_core::cart::BackupKind;
use sdl2::event::Event;
use sdl2::keyboard::{Keycode, Scancode};
use sdl2::mouse::MouseButton;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use video::SCREEN_GAP;

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

    /// Force AUXSPI backup type. One of:
    /// `none`, `eeprom-512b`, `eeprom-8k`, `eeprom-64k`,
    /// `fram-32k`, `flash-256k`, `flash-512k`, `flash-1m`.
    /// Default: `eeprom-64k` (heuristic).
    #[arg(long)]
    save_type: Option<String>,
}

fn parse_backup_kind(s: &str) -> Option<BackupKind> {
    Some(match s {
        "none"         => BackupKind::None,
        "eeprom-512b"  => BackupKind::Eeprom512B,
        "eeprom-8k"    => BackupKind::Eeprom8K,
        "eeprom-64k"   => BackupKind::Eeprom64K,
        "fram-32k"     => BackupKind::Fram32K,
        "flash-256k"   => BackupKind::Flash256K,
        "flash-512k"   => BackupKind::Flash512K,
        "flash-1m"     => BackupKind::Flash1M,
        _ => return None,
    })
}

/// Translate a window-pixel mouse coordinate to an NDS-screen (bottom-screen)
/// coordinate. Returns `None` if the mouse is outside the bottom screen.
fn mouse_to_touch(mx: i32, my: i32, scale: u32) -> Option<(u16, u16)> {
    let scale = scale as i32;
    let top_h = SCREEN_HEIGHT as i32 * scale;
    let gap = SCREEN_GAP as i32 * scale;
    let bot_top = top_h + gap;
    let bot_bottom = bot_top + SCREEN_HEIGHT as i32 * scale;
    let win_w = SCREEN_WIDTH as i32 * scale;
    if mx < 0 || mx >= win_w { return None; }
    if my < bot_top || my >= bot_bottom { return None; }
    let x = (mx / scale) as u16;
    let y = ((my - bot_top) / scale) as u16;
    Some((x, y))
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

    // Resolve AUXSPI backup type — explicit `--save-type`, then heuristic
    // from the ROM header, else default to 64 KB EEPROM.
    let mut save_path: Option<PathBuf> = None;
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

                            // Backup-type resolution.
                            let kind = match args.save_type.as_deref() {
                                Some(s) => match parse_backup_kind(s) {
                                    Some(k) => k,
                                    None => {
                                        eprintln!("warning: unknown --save-type '{}', using header heuristic", s);
                                        BackupKind::guess_from_header(h.device_capacity)
                                    }
                                },
                                None => BackupKind::guess_from_header(h.device_capacity),
                            };
                            eprintln!("Backup type: {:?} ({} bytes)", kind, kind.size());
                            nds.set_backup_kind(kind);

                            // Auto-load .sav next to the ROM (read-only stage; saves
                            // back on exit).
                            let p = rom.with_extension("sav");
                            if let Ok(data) = fs::read(&p) {
                                eprintln!("Loaded save: {}", p.display());
                                nds.import_save(&data);
                            }
                            save_path = Some(p);
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

    // Optional firmware dump — overrides the synthesized image.
    if let Some(fw_path) = &args.firmware {
        match fs::read(fw_path) {
            Ok(bytes) => {
                eprintln!("Loaded firmware: {} ({} bytes)", fw_path.display(), bytes.len());
                nds.shared.spi.firmware.load_dump(&bytes);
            }
            Err(e) => eprintln!("warning: could not read firmware: {}", e),
        }
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

        // Mouse → touch on the bottom screen.
        let mouse = events.mouse_state();
        let left_down = mouse.is_mouse_button_pressed(MouseButton::Left);
        match (left_down, mouse_to_touch(mouse.x(), mouse.y(), args.scale)) {
            (true, Some((tx, ty))) => nds.set_touch(tx, ty, true),
            _ => nds.set_touch(0, 0, false),
        }

        nds.run_frame();
        display.present(&nds.framebuffer_top, &nds.framebuffer_bot);

        let elapsed = frame_start.elapsed();
        if elapsed < frame_target {
            std::thread::sleep(frame_target - elapsed);
        }
    }

    // Export AUXSPI backup to .sav on exit.
    if let (Some(sav), Some(data)) = (save_path.as_deref(), nds.export_save()) {
        match fs::write(sav, &data) {
            Ok(()) => eprintln!("Saved {} bytes to {}", data.len(), sav.display()),
            Err(e) => eprintln!("warning: failed to save: {}", e),
        }
    }

    eprintln!("nds-frontend: exiting");
}

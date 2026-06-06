//! NDS emulator frontend (Phase 1 skeleton).
//!
//! Opens a single SDL2 window sized for two stacked 256x192 screens and
//! steps the core.

mod audio;
mod video;

use clap::Parser;
use nds_core::cart::{BackupKind, CartHeader};
use nds_core::{Nds, SCREEN_HEIGHT, SCREEN_WIDTH};
use sdl2::event::Event;
use sdl2::keyboard::{Keycode, Scancode};
use sdl2::mouse::MouseButton;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use video::DEFAULT_SCREEN_GAP;

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

    /// Vertical gap between screens, measured in native DS pixels.
    #[arg(long, default_value_t = DEFAULT_SCREEN_GAP)]
    screen_gap: u32,

    /// Force AUXSPI backup type. One of:
    /// `none`, `eeprom-512b`, `eeprom-8k`, `eeprom-64k`,
    /// `fram-32k`, `flash-256k`, `flash-512k`, `flash-1m`.
    /// Default: `eeprom-64k` (heuristic).
    #[arg(long)]
    save_type: Option<String>,

    /// Disable audio output.
    #[arg(long)]
    no_audio: bool,

    /// Run this many frames, write a native-size dual-screen PPM, then exit.
    #[arg(long, value_name = "PATH")]
    capture_ppm: Option<PathBuf>,

    /// Write numbered native-size PPM captures to this directory.
    #[arg(long, value_name = "DIR")]
    capture_dir: Option<PathBuf>,

    /// Number of frames to run before writing `--capture-ppm`.
    #[arg(long, default_value_t = 600)]
    capture_frames: u32,

    /// Frame interval for `--capture-dir` sequence captures.
    #[arg(long, default_value_t = 60)]
    capture_interval: u32,

    /// Diagnostic: skip the 3D anti-alias post-pass.
    #[arg(long, hide = true)]
    debug_disable_3d_aa: bool,
}

fn save_state_path(rom: Option<&Path>) -> Option<PathBuf> {
    rom.map(|p| p.with_extension("state"))
}

/// F5 — serialize the entire emulator, zstd-compress, write to `.state`.
fn save_state(nds: &Nds, path: &Path) {
    let bytes = match bincode::serialize(nds) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("save_state: serialize failed: {}", e);
            return;
        }
    };
    let compressed = match zstd::encode_all(bytes.as_slice(), 3) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("save_state: compress failed: {}", e);
            return;
        }
    };
    match fs::write(path, &compressed) {
        Ok(()) => eprintln!(
            "Save state written to {} ({} → {} bytes)",
            path.display(),
            bytes.len(),
            compressed.len()
        ),
        Err(e) => eprintln!("save_state: write failed: {}", e),
    }
}

/// F8 — read `.state`, zstd-decompress, deserialize, assign in place.
fn load_state(nds: &mut Nds, path: &Path) {
    if !path.exists() {
        eprintln!("No save state at {}", path.display());
        return;
    }
    let compressed = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("load_state: read failed: {}", e);
            return;
        }
    };
    let bytes = match zstd::decode_all(compressed.as_slice()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("load_state: decompress failed: {}", e);
            return;
        }
    };
    let restored: Nds = match bincode::deserialize(&bytes) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("load_state: deserialize failed: {}", e);
            return;
        }
    };
    *nds = restored;
    eprintln!("Loaded save state from {}", path.display());
}

fn parse_backup_kind(s: &str) -> Option<BackupKind> {
    Some(match s {
        "none" => BackupKind::None,
        "eeprom-512b" => BackupKind::Eeprom512B,
        "eeprom-8k" => BackupKind::Eeprom8K,
        "eeprom-64k" => BackupKind::Eeprom64K,
        "fram-32k" => BackupKind::Fram32K,
        "flash-256k" => BackupKind::Flash256K,
        "flash-512k" => BackupKind::Flash512K,
        "flash-1m" => BackupKind::Flash1M,
        _ => return None,
    })
}

/// Translate a window-pixel mouse coordinate to an NDS-screen (bottom-screen)
/// coordinate. Returns `None` if the mouse is outside the bottom screen.
fn mouse_to_touch(mx: i32, my: i32, scale: u32, screen_gap: u32) -> Option<(u16, u16)> {
    let scale = scale as i32;
    let top_h = SCREEN_HEIGHT as i32 * scale;
    let gap = screen_gap as i32 * scale;
    let bot_top = top_h + gap;
    let bot_bottom = bot_top + SCREEN_HEIGHT as i32 * scale;
    let win_w = SCREEN_WIDTH as i32 * scale;
    if mx < 0 || mx >= win_w {
        return None;
    }
    if my < bot_top || my >= bot_bottom {
        return None;
    }
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

fn bgr555_to_rgb888(color: u16) -> [u8; 3] {
    let expand = |v: u16| -> u8 {
        let v = (v & 0x1F) as u8;
        (v << 3) | (v >> 2)
    };
    [expand(color), expand(color >> 5), expand(color >> 10)]
}

fn write_capture_ppm(
    path: &Path,
    top: &[u16],
    bottom: &[u16],
    screen_gap: u32,
) -> std::io::Result<()> {
    let gap = screen_gap as usize;
    let width = SCREEN_WIDTH;
    let height = SCREEN_HEIGHT * 2 + gap;
    let mut out = Vec::with_capacity(width * height * 3 + 64);
    out.extend_from_slice(format!("P6\n{} {}\n255\n", width, height).as_bytes());

    for y in 0..SCREEN_HEIGHT {
        for x in 0..SCREEN_WIDTH {
            out.extend_from_slice(&bgr555_to_rgb888(top[y * SCREEN_WIDTH + x]));
        }
    }
    for _ in 0..gap {
        for _ in 0..SCREEN_WIDTH {
            out.extend_from_slice(&[0, 0, 0]);
        }
    }
    for y in 0..SCREEN_HEIGHT {
        for x in 0..SCREEN_WIDTH {
            out.extend_from_slice(&bgr555_to_rgb888(bottom[y * SCREEN_WIDTH + x]));
        }
    }

    fs::write(path, out)
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn capture_sequence_frames(capture_frames: u32, capture_interval: u32) -> Vec<u32> {
    let interval = capture_interval.max(1);
    (1..=capture_frames)
        .filter(|frame| frame % interval == 0)
        .collect()
}

fn write_capture_metadata(
    path: &Path,
    rom: Option<&Path>,
    rom_size: Option<usize>,
    header: Option<&CartHeader>,
    capture_frames: u32,
    capture_interval: u32,
    screen_gap: u32,
    sequence: bool,
) -> std::io::Result<()> {
    let interval = capture_interval.max(1);
    let output_width = SCREEN_WIDTH;
    let output_height = SCREEN_HEIGHT * 2 + screen_gap as usize;
    let rom_json = rom
        .map(|p| format!("\"{}\"", json_escape(&p.display().to_string())))
        .unwrap_or_else(|| "null".to_string());
    let rom_size_json = rom_size
        .map(|size| size.to_string())
        .unwrap_or_else(|| "null".to_string());
    let (title_json, gamecode_json, header_crc_valid_json) = if let Some(h) = header {
        (
            format!("\"{}\"", json_escape(&h.title)),
            format!("\"{}\"", json_escape(&h.gamecode_str())),
            h.header_crc_valid().to_string(),
        )
    } else {
        ("null".to_string(), "null".to_string(), "null".to_string())
    };
    let frame_entries = if sequence {
        capture_sequence_frames(capture_frames, interval)
            .iter()
            .map(|frame| format!("\"frame-{:06}.ppm\"", frame))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        String::new()
    };
    let capture_kind = if sequence { "sequence" } else { "single" };
    let json = format!(
        concat!(
            "{{\n",
            "  \"format\": \"nds-frontend-capture-v1\",\n",
            "  \"kind\": \"{}\",\n",
            "  \"rom\": {},\n",
            "  \"rom_size\": {},\n",
            "  \"rom_title\": {},\n",
            "  \"gamecode\": {},\n",
            "  \"header_crc_valid\": {},\n",
            "  \"capture_frames\": {},\n",
            "  \"capture_interval\": {},\n",
            "  \"screen_gap\": {},\n",
            "  \"screen_width\": {},\n",
            "  \"screen_height\": {},\n",
            "  \"output_width\": {},\n",
            "  \"output_height\": {},\n",
            "  \"frame_files\": [{}]\n",
            "}}\n"
        ),
        capture_kind,
        rom_json,
        rom_size_json,
        title_json,
        gamecode_json,
        header_crc_valid_json,
        capture_frames,
        interval,
        screen_gap,
        SCREEN_WIDTH,
        SCREEN_HEIGHT,
        output_width,
        output_height,
        frame_entries
    );
    fs::write(path, json)
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
                                if h.header_crc_valid() {
                                    "valid"
                                } else {
                                    "INVALID"
                                }
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
                eprintln!(
                    "Loaded firmware: {} ({} bytes)",
                    fw_path.display(),
                    bytes.len()
                );
                nds.shared.spi.firmware.load_dump(&bytes);
            }
            Err(e) => eprintln!("warning: could not read firmware: {}", e),
        }
    }

    nds.shared.gpu3d.rasterizer.debug_disable_antialiasing = args.debug_disable_3d_aa;

    if args.capture_ppm.is_some() || args.capture_dir.is_some() {
        if let Some(dir) = &args.capture_dir {
            if let Err(e) = fs::create_dir_all(dir) {
                eprintln!("warning: failed to create capture dir: {}", e);
                return;
            }
        }
        let interval = args.capture_interval.max(1);
        for frame in 1..=args.capture_frames {
            nds.run_frame();
            if let Some(dir) = &args.capture_dir {
                if frame % interval == 0 {
                    let path = dir.join(format!("frame-{:06}.ppm", frame));
                    if let Err(e) = write_capture_ppm(
                        &path,
                        &nds.framebuffer_top,
                        &nds.framebuffer_bot,
                        args.screen_gap,
                    ) {
                        eprintln!("warning: failed to write {}: {}", path.display(), e);
                        return;
                    }
                }
            }
        }
        if let Some(capture_path) = &args.capture_ppm {
            match write_capture_ppm(
                capture_path,
                &nds.framebuffer_top,
                &nds.framebuffer_bot,
                args.screen_gap,
            ) {
                Ok(()) => eprintln!(
                    "Captured frame {} to {}",
                    args.capture_frames,
                    capture_path.display()
                ),
                Err(e) => eprintln!("warning: failed to write capture: {}", e),
            }
            let metadata_path = capture_path.with_extension("json");
            if let Err(e) = write_capture_metadata(
                &metadata_path,
                args.rom.as_deref(),
                nds.cart.rom().map(|rom| rom.len()),
                nds.cart.header(),
                args.capture_frames,
                args.capture_interval,
                args.screen_gap,
                false,
            ) {
                eprintln!(
                    "warning: failed to write capture metadata {}: {}",
                    metadata_path.display(),
                    e
                );
            }
        }
        if let Some(dir) = &args.capture_dir {
            let metadata_path = dir.join("capture-metadata.json");
            if let Err(e) = write_capture_metadata(
                &metadata_path,
                args.rom.as_deref(),
                nds.cart.rom().map(|rom| rom.len()),
                nds.cart.header(),
                args.capture_frames,
                interval,
                args.screen_gap,
                true,
            ) {
                eprintln!(
                    "warning: failed to write capture metadata {}: {}",
                    metadata_path.display(),
                    e
                );
            }
            eprintln!(
                "Captured frame sequence through frame {} to {} every {} frames",
                args.capture_frames,
                dir.display(),
                interval
            );
        }
        return;
    }

    let sdl = sdl2::init().expect("failed to init SDL2");
    let mut display = video::DualScreen::new(&sdl, args.scale, args.screen_gap);
    let audio_out = if args.no_audio {
        None
    } else {
        audio::AudioOutput::new(&sdl)
    };
    let mut events = sdl.event_pump().expect("event pump");
    let state_path = save_state_path(args.rom.as_deref());

    // Frontend-side audio drain buffer.
    let mut audio_buf = vec![0i16; 4096];

    let frame_target = Duration::from_micros(16_715); // ~59.83 Hz
    'main_loop: loop {
        let frame_start = Instant::now();

        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => break 'main_loop,
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => break 'main_loop,
                Event::KeyDown {
                    keycode: Some(Keycode::F5),
                    ..
                } => {
                    if let Some(p) = &state_path {
                        save_state(&nds, p);
                    } else {
                        eprintln!("Save state needs a --rom path to derive .state file");
                    }
                }
                Event::KeyDown {
                    keycode: Some(Keycode::F8),
                    ..
                } => {
                    if let Some(p) = &state_path {
                        load_state(&mut nds, p);
                    } else {
                        eprintln!("Load state needs a --rom path to derive .state file");
                    }
                }
                _ => {}
            }
        }

        // Sample keyboard, translate to KEYINPUT bits.
        let kb = events.keyboard_state();
        let pressed = |sc: Scancode| kb.is_scancode_pressed(sc);
        let mut keys = 0u16;
        // KEYINPUT is active-low: bit = 1 means released.
        if !pressed(Scancode::Z) {
            keys |= 1 << 0;
        } else {
        } // A
        if !pressed(Scancode::X) {
            keys |= 1 << 1;
        } // B
        if !pressed(Scancode::RShift) {
            keys |= 1 << 2;
        } // Select
        if !pressed(Scancode::Return) {
            keys |= 1 << 3;
        } // Start
        if !pressed(Scancode::Right) {
            keys |= 1 << 4;
        }
        if !pressed(Scancode::Left) {
            keys |= 1 << 5;
        }
        if !pressed(Scancode::Up) {
            keys |= 1 << 6;
        }
        if !pressed(Scancode::Down) {
            keys |= 1 << 7;
        }
        if !pressed(Scancode::S) {
            keys |= 1 << 8;
        } // R shoulder
        if !pressed(Scancode::A) {
            keys |= 1 << 9;
        } // L shoulder
        nds.set_keys(keys);

        // Mouse → touch on the bottom screen.
        let mouse = events.mouse_state();
        let left_down = mouse.is_mouse_button_pressed(MouseButton::Left);
        match (
            left_down,
            mouse_to_touch(mouse.x(), mouse.y(), args.scale, args.screen_gap),
        ) {
            (true, Some((tx, ty))) => nds.set_touch(tx, ty, true),
            _ => nds.set_touch(0, 0, false),
        }

        nds.run_frame();
        display.present(&nds.framebuffer_top, &nds.framebuffer_bot);

        // Pump audio samples from the core to the SDL2 callback queue.
        if let Some(out) = &audio_out {
            let n = nds.drain_audio(&mut audio_buf);
            if n > 0 {
                out.push(&audio_buf[..n]);
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bgr555_to_rgb888_expands_channels() {
        assert_eq!(bgr555_to_rgb888(0x001F), [255, 0, 0]);
        assert_eq!(bgr555_to_rgb888(0x03E0), [0, 255, 0]);
        assert_eq!(bgr555_to_rgb888(0x7C00), [0, 0, 255]);
    }

    #[test]
    fn test_capture_ppm_layout_size() {
        let path =
            std::env::temp_dir().join(format!("nds_capture_test_{}.ppm", std::process::id()));
        let top = vec![0x001F; SCREEN_WIDTH * SCREEN_HEIGHT];
        let bottom = vec![0x03E0; SCREEN_WIDTH * SCREEN_HEIGHT];

        write_capture_ppm(&path, &top, &bottom, 8).expect("write capture");

        let bytes = fs::read(&path).expect("read capture");
        let header = format!("P6\n{} {}\n255\n", SCREEN_WIDTH, SCREEN_HEIGHT * 2 + 8);
        assert!(bytes.starts_with(header.as_bytes()));
        assert_eq!(
            bytes.len(),
            header.len() + SCREEN_WIDTH * (SCREEN_HEIGHT * 2 + 8) * 3
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_capture_sequence_frames_respects_interval_floor() {
        assert_eq!(capture_sequence_frames(10, 3), vec![3, 6, 9]);
        assert_eq!(capture_sequence_frames(3, 0), vec![1, 2, 3]);
    }

    #[test]
    fn test_capture_metadata_lists_sequence_frames_and_layout() {
        let path = std::env::temp_dir().join(format!(
            "nds_capture_metadata_test_{}.json",
            std::process::id()
        ));

        write_capture_metadata(
            &path,
            Some(Path::new("/tmp/test\"rom.nds")),
            Some(1024),
            None,
            180,
            60,
            8,
            true,
        )
        .expect("write metadata");

        let json = fs::read_to_string(&path).expect("read metadata");
        assert!(json.contains("\"format\": \"nds-frontend-capture-v1\""));
        assert!(json.contains("\"kind\": \"sequence\""));
        assert!(json.contains("\"rom\": \"/tmp/test\\\"rom.nds\""));
        assert!(json.contains("\"rom_size\": 1024"));
        assert!(json.contains("\"rom_title\": null"));
        assert!(json.contains("\"gamecode\": null"));
        assert!(json.contains("\"header_crc_valid\": null"));
        assert!(json.contains("\"capture_frames\": 180"));
        assert!(json.contains("\"capture_interval\": 60"));
        assert!(json.contains("\"screen_gap\": 8"));
        assert!(json.contains("\"output_width\": 256"));
        assert!(json.contains("\"output_height\": 392"));
        assert!(json.contains("\"frame-000060.ppm\""));
        assert!(json.contains("\"frame-000120.ppm\""));
        assert!(json.contains("\"frame-000180.ppm\""));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_capture_args_accept_sequence_options() {
        let args = Args::parse_from([
            "nds-frontend",
            "--capture-dir",
            "/tmp/nds-captures",
            "--capture-frames",
            "180",
            "--capture-interval",
            "30",
        ]);

        assert_eq!(args.capture_dir, Some(PathBuf::from("/tmp/nds-captures")));
        assert_eq!(args.capture_frames, 180);
        assert_eq!(args.capture_interval, 30);
        assert!(args.capture_ppm.is_none());
    }

    #[test]
    fn test_screen_gap_defaults_to_compact_layout() {
        let args = Args::parse_from(["nds-frontend"]);

        assert_eq!(args.screen_gap, 0);
    }
}

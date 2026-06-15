use nds_core::cart::BackupKind;
use nds_core::{Nds, SCREEN_HEIGHT, SCREEN_WIDTH};
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const AUDIO_RATE: u32 = 32768;
const AUDIO_CHANNELS: u32 = 2;

pub fn run() -> Result<(), String> {
    let mut harness = Eharness::new();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();

    loop {
        let (req, blob) = read_frame(&mut input)?;
        let cmd = req
            .get("cmd")
            .and_then(Value::as_str)
            .ok_or_else(|| "request missing string cmd".to_string())?;
        let result = harness.handle(cmd, &req, &blob);
        match result {
            Ok((resp, resp_blob, should_exit)) => {
                write_frame(&mut output, &resp, &resp_blob)?;
                output.flush().map_err(|e| e.to_string())?;
                if should_exit {
                    return Ok(());
                }
            }
            Err(e) => {
                write_frame(&mut output, &json!({"ok": false, "error": e}), &[])?;
                output.flush().map_err(|e| e.to_string())?;
            }
        }
    }
}

struct Eharness {
    nds: Nds,
    bios9: Option<Vec<u8>>,
    bios7: Option<Vec<u8>>,
    rom_path: Option<PathBuf>,
    rom_bytes: Option<Vec<u8>>,
    save_bytes: Option<Vec<u8>>,
    frame_index: u64,
    buttons: u32,
    touch: Option<(u16, u16, bool)>,
    audio: Vec<i16>,
}

impl Eharness {
    fn new() -> Self {
        Self {
            nds: Nds::new(None, None),
            bios9: None,
            bios7: None,
            rom_path: None,
            rom_bytes: None,
            save_bytes: None,
            frame_index: 0,
            buttons: 0,
            touch: None,
            audio: Vec::new(),
        }
    }

    fn handle(
        &mut self,
        cmd: &str,
        req: &Value,
        blob: &[u8],
    ) -> Result<(Value, Vec<u8>, bool), String> {
        match cmd {
            "hello" => Ok((self.hello(), Vec::new(), false)),
            "load_rom" => {
                let path = required_path(req)?;
                self.load_rom(&path)?;
                Ok((json!({"ok": true}), Vec::new(), false))
            }
            "load_bios" => {
                let path = required_path(req)?;
                let slot = req.get("slot").and_then(Value::as_str).unwrap_or("arm9");
                let bytes =
                    fs::read(&path).map_err(|e| format!("read BIOS {}: {}", path.display(), e))?;
                match slot {
                    "arm9" => self.bios9 = Some(bytes),
                    "arm7" => self.bios7 = Some(bytes),
                    _ => return Err(format!("unknown BIOS slot {slot:?}")),
                }
                self.reset()?;
                Ok((json!({"ok": true}), Vec::new(), false))
            }
            "load_save" => {
                let path = required_path(req)?;
                let bytes =
                    fs::read(&path).map_err(|e| format!("read save {}: {}", path.display(), e))?;
                self.save_bytes = Some(bytes.clone());
                self.nds.import_save(&bytes);
                Ok((json!({"ok": true}), Vec::new(), false))
            }
            "reset" => {
                self.reset()?;
                Ok((json!({"ok": true}), Vec::new(), false))
            }
            "set_input" => {
                self.buttons = req.get("buttons").and_then(Value::as_u64).unwrap_or(0) as u32;
                self.touch = parse_touch(req.get("touch"))?;
                self.apply_input();
                Ok((json!({"ok": true}), Vec::new(), false))
            }
            "step" => {
                let frames = req
                    .get("frames")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .max(1);
                self.step(frames as usize);
                Ok((
                    json!({"ok": true, "frame_index": self.frame_index}),
                    Vec::new(),
                    false,
                ))
            }
            "get_video" => self.get_video(req),
            "get_audio" => Ok((self.get_audio_header(), self.take_audio_blob(), false)),
            "save_state" => {
                let bytes = bincode::serialize(&self.nds).map_err(|e| e.to_string())?;
                Ok((json!({"ok": true}), bytes, false))
            }
            "load_state" => {
                self.nds = bincode::deserialize(blob).map_err(|e| e.to_string())?;
                self.apply_input();
                Ok((json!({"ok": true}), Vec::new(), false))
            }
            "peek" => Err("peek is not implemented by the NDS harness yet".to_string()),
            "bye" => Ok((json!({"ok": true}), Vec::new(), true)),
            _ => Err(format!("unknown command {cmd:?}")),
        }
    }

    fn hello(&self) -> Value {
        json!({
            "ok": true,
            "engine": "nds",
            "version": env!("CARGO_PKG_VERSION"),
            "screens": [
                {"index": 0, "w": SCREEN_WIDTH, "h": SCREEN_HEIGHT, "fmt": "BGR555"},
                {"index": 1, "w": SCREEN_WIDTH, "h": SCREEN_HEIGHT, "fmt": "BGR555"}
            ],
            "audio": {"rate": AUDIO_RATE, "channels": AUDIO_CHANNELS, "fmt": "s16le"},
            "buttons": ["A", "B", "Select", "Start", "Right", "Left", "Up", "Down", "R", "L", "X", "Y"],
            "has_touch": true,
            "has_extkeys": true,
            "peek": false
        })
    }

    fn load_rom(&mut self, path: &Path) -> Result<(), String> {
        let bytes = fs::read(path).map_err(|e| format!("read ROM {}: {}", path.display(), e))?;
        self.rom_path = Some(path.to_path_buf());
        self.rom_bytes = Some(bytes);
        if self.save_bytes.is_none() {
            let sav = path.with_extension("sav");
            if let Ok(bytes) = fs::read(&sav) {
                eprintln!("harness: loaded save {}", sav.display());
                self.save_bytes = Some(bytes);
            }
        }
        self.reset()
    }

    fn reset(&mut self) -> Result<(), String> {
        self.nds = Nds::new(self.bios9.clone(), self.bios7.clone());
        if let Some(bytes) = self.rom_bytes.clone() {
            self.nds
                .load_cart_direct_boot(bytes)
                .map_err(|e| format!("direct boot failed: {e}"))?;
            if let Some(header) = self.nds.cart.header() {
                let kind = BackupKind::guess_from_header(header.device_capacity);
                self.nds.set_backup_kind(kind);
            }
        }
        if let Some(save) = &self.save_bytes {
            self.nds.import_save(save);
        }
        self.frame_index = 0;
        self.audio.clear();
        self.apply_input();
        Ok(())
    }

    fn apply_input(&mut self) {
        let mut keyinput = 0x03FFu16;
        for (canonical, key_bit) in [
            (0, 0), // A
            (1, 1), // B
            (2, 2), // Select
            (3, 3), // Start
            (4, 4), // Right
            (5, 5), // Left
            (6, 6), // Up
            (7, 7), // Down
            (8, 8), // R
            (9, 9), // L
        ] {
            if self.buttons & (1 << canonical) != 0 {
                keyinput &= !(1 << key_bit);
            }
        }
        self.nds.set_keys(keyinput);

        let mut extkeyin = 0x007F;
        if self.buttons & (1 << 10) != 0 {
            extkeyin &= !(1 << 0); // X
        }
        if self.buttons & (1 << 11) != 0 {
            extkeyin &= !(1 << 1); // Y
        }
        self.nds.set_extkeys(extkeyin);

        match self.touch {
            Some((x, y, true)) => self.nds.set_touch(x, y, true),
            _ => self.nds.set_touch(0, 0, false),
        }
    }

    fn step(&mut self, frames: usize) {
        let mut audio_buf = vec![0i16; 4096];
        for _ in 0..frames {
            self.apply_input();
            self.nds.run_frame();
            self.frame_index += 1;
            loop {
                let n = self.nds.drain_audio(&mut audio_buf);
                if n == 0 {
                    break;
                }
                self.audio.extend_from_slice(&audio_buf[..n]);
                if n < audio_buf.len() {
                    break;
                }
            }
        }
    }

    fn get_video(&self, req: &Value) -> Result<(Value, Vec<u8>, bool), String> {
        let requested = req.get("screen").and_then(Value::as_i64);
        let mut screens = Vec::new();
        let mut blob = Vec::new();
        for (index, fb) in [
            (0usize, self.nds.framebuffer_top.as_slice()),
            (1usize, self.nds.framebuffer_bot.as_slice()),
        ] {
            if requested.is_some_and(|screen| screen != index as i64) {
                continue;
            }
            let offset = blob.len();
            append_bgr555(&mut blob, fb);
            screens.push(json!({
                "index": index,
                "w": SCREEN_WIDTH,
                "h": SCREEN_HEIGHT,
                "fmt": "BGR555",
                "offset": offset,
                "len": fb.len() * 2
            }));
        }
        Ok((json!({"ok": true, "screens": screens}), blob, false))
    }

    fn get_audio_header(&self) -> Value {
        json!({
            "ok": true,
            "rate": AUDIO_RATE,
            "channels": AUDIO_CHANNELS,
            "fmt": "s16le",
            "nsamples": self.audio.len() / AUDIO_CHANNELS as usize
        })
    }

    fn take_audio_blob(&mut self) -> Vec<u8> {
        let mut blob = Vec::with_capacity(self.audio.len() * 2);
        for sample in self.audio.drain(..) {
            blob.extend_from_slice(&sample.to_le_bytes());
        }
        blob
    }
}

fn parse_touch(value: Option<&Value>) -> Result<Option<(u16, u16, bool)>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let x = value
        .get("x")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min((SCREEN_WIDTH - 1) as u64);
    let y = value
        .get("y")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min((SCREEN_HEIGHT - 1) as u64);
    let down = value.get("down").and_then(Value::as_bool).unwrap_or(false);
    Ok(Some((x as u16, y as u16, down)))
}

fn required_path(req: &Value) -> Result<PathBuf, String> {
    req.get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "request missing path".to_string())
}

fn append_bgr555(blob: &mut Vec<u8>, fb: &[u16]) {
    blob.reserve(fb.len() * 2);
    for pixel in fb {
        blob.extend_from_slice(&pixel.to_le_bytes());
    }
}

fn read_frame(input: &mut impl Read) -> Result<(Value, Vec<u8>), String> {
    let mut hdr = [0u8; 8];
    input.read_exact(&mut hdr).map_err(|e| e.to_string())?;
    let total_len = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
    let json_len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
    if total_len < 4 || json_len > total_len - 4 {
        return Err(format!(
            "invalid frame lengths total={total_len} json={json_len}"
        ));
    }
    let mut json_bytes = vec![0u8; json_len];
    input
        .read_exact(&mut json_bytes)
        .map_err(|e| e.to_string())?;
    let blob_len = total_len - 4 - json_len;
    let mut blob = vec![0u8; blob_len];
    input.read_exact(&mut blob).map_err(|e| e.to_string())?;
    let value = serde_json::from_slice(&json_bytes).map_err(|e| e.to_string())?;
    Ok((value, blob))
}

fn write_frame(output: &mut impl Write, header: &Value, blob: &[u8]) -> Result<(), String> {
    let json_bytes = serde_json::to_vec(header).map_err(|e| e.to_string())?;
    let total_len = 4usize
        .checked_add(json_bytes.len())
        .and_then(|n| n.checked_add(blob.len()))
        .ok_or_else(|| "frame too large".to_string())?;
    let total_len = u32::try_from(total_len).map_err(|_| "frame too large".to_string())?;
    let json_len = u32::try_from(json_bytes.len()).map_err(|_| "json too large".to_string())?;
    output
        .write_all(&total_len.to_le_bytes())
        .and_then(|_| output.write_all(&json_len.to_le_bytes()))
        .and_then(|_| output.write_all(&json_bytes))
        .and_then(|_| output.write_all(blob))
        .map_err(|e| e.to_string())
}

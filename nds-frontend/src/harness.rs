use nds_core::cart::BackupKind;
use nds_core::gpu3d::raster::texture::{self, TexParams};
use nds_core::vram::{BankId, VramRouter, VramTarget};
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
            "set_debug_layers" => {
                if let Some(disable_obj) = req.get("disable_2d_obj").and_then(Value::as_bool) {
                    self.nds.debug_disable_2d_obj = disable_obj;
                }
                if let Some(disable_bgs) = req.get("disable_2d_bg").and_then(Value::as_array) {
                    self.nds.debug_disable_2d_bg = [false; 4];
                    for bg in disable_bgs {
                        let Some(bg) = bg.as_u64() else {
                            return Err("disable_2d_bg entries must be integers".to_string());
                        };
                        if bg < 4 {
                            self.nds.debug_disable_2d_bg[bg as usize] = true;
                        }
                    }
                }
                Ok((
                    json!({
                        "ok": true,
                        "disable_2d_obj": self.nds.debug_disable_2d_obj,
                        "disable_2d_bg": self.nds.debug_disable_2d_bg,
                    }),
                    Vec::new(),
                    false,
                ))
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
            "get_3d_video" => self.get_3d_video(),
            "get_3d_debug" => Ok((self.get_3d_debug(req), Vec::new(), false)),
            "get_2d_obj_debug" => Ok((self.get_2d_obj_debug(req), Vec::new(), false)),
            "dump_texture_image_raw" => self.dump_texture_image_raw(req),
            "dump_texture_palette_raw" => self.dump_texture_palette_raw(req),
            "dump_main_ram_raw" => self.dump_main_ram_raw(req),
            "poke_main_ram_raw" => self.poke_main_ram_raw(req, &blob),
            "dump_vram_bank_raw" => self.dump_vram_bank_raw(req),
            "rerender_3d_debug" => {
                self.rerender_3d_debug(req)?;
                Ok((json!({"ok": true}), Vec::new(), false))
            }
            "dump_3d_texture" => self.dump_3d_texture(req),
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

    fn get_3d_video(&self) -> Result<(Value, Vec<u8>, bool), String> {
        let mut blob = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT * 2);
        for &px in &self.nds.shared.gpu3d.rasterizer.framebuffer {
            blob.extend_from_slice(&px.to_le_bytes());
        }
        Ok((
            json!({
                "ok": true,
                "screen": {"index": 0, "w": SCREEN_WIDTH, "h": SCREEN_HEIGHT, "fmt": "BGR555", "offset": 0, "len": blob.len()}
            }),
            blob,
            false,
        ))
    }

    fn get_3d_debug(&self, req: &Value) -> Value {
        let x = req.get("x").and_then(Value::as_i64).unwrap_or(96) as i32;
        let y = req.get("y").and_then(Value::as_i64).unwrap_or(48) as i32;
        let w = req.get("w").and_then(Value::as_i64).unwrap_or(72) as i32;
        let h = req.get("h").and_then(Value::as_i64).unwrap_or(80) as i32;
        let limit = req.get("limit").and_then(Value::as_u64).unwrap_or(64) as usize;
        let rect = (x, y, x + w, y + h);
        let gpu3d = &self.nds.shared.gpu3d;
        let rast = &gpu3d.rasterizer;

        let mut polys = Vec::new();
        for (index, p) in gpu3d.raster_polygons.iter().enumerate() {
            if p.vertices.is_empty() {
                continue;
            }
            let mut min_x = i32::MAX;
            let mut min_y = i32::MAX;
            let mut max_x = i32::MIN;
            let mut max_y = i32::MIN;
            let mut min_z = i32::MAX;
            let mut max_z = i32::MIN;
            let mut min_w = i32::MAX;
            let mut max_w = i32::MIN;
            for v in &p.vertices {
                let sx = v.screen_x >> 8;
                let sy = v.screen_y >> 8;
                min_x = min_x.min(sx);
                min_y = min_y.min(sy);
                max_x = max_x.max(sx);
                max_y = max_y.max(sy);
                min_z = min_z.min(v.depth_z);
                max_z = max_z.max(v.depth_z);
                min_w = min_w.min(v.w);
                max_w = max_w.max(v.w);
            }
            if max_x < rect.0 || min_x >= rect.2 || max_y < rect.1 || min_y >= rect.3 {
                continue;
            }
            polys.push(json!({
                "index": index,
                "bbox": [min_x, min_y, max_x, max_y],
                "attr": p.attr,
                "alpha": (p.attr >> 16) & 0x1F,
                "mode": (p.attr >> 4) & 0x3,
                "poly_id": (p.attr >> 24) & 0x3F,
                "depth_equal": p.attr & (1 << 14) != 0,
                "translucent_depth_update": p.attr & (1 << 11) != 0,
                "render_back": p.attr & (1 << 6) != 0,
                "render_front": p.attr & (1 << 7) != 0,
                "tex": p.tex_image_param,
                "tex_format": (p.tex_image_param >> 26) & 0x7,
                "palette_base": p.palette_base,
                "texture_snapshot": p.texture_snapshot.as_ref().map(|snapshot| {
                    json!({
                        "image_len": snapshot.image_len(),
                        "image_nonzero_nibbles": snapshot.image_nonzero_nibbles(),
                    })
                }),
                "vertices": p.vertices.len(),
                "vertex_data": p.vertices.iter().map(|v| {
                    json!({
                        "screen": [v.screen_x, v.screen_y],
                        "pixel": [v.screen_x >> 8, v.screen_y >> 8],
                        "z": v.depth_z,
                        "w": v.w,
                        "color": v.color,
                        "tex": v.tex,
                    })
                }).collect::<Vec<_>>(),
                "z": [min_z, max_z],
                "w": [min_w, max_w],
            }));
            if polys.len() >= limit {
                break;
            }
        }

        let mut samples = Vec::new();
        for (sx, sy) in [
            (x + w / 2, y + h / 2),
            (x + w / 2, y + h * 2 / 3),
            (x + w / 2, y + h / 3),
            (x + w / 3, y + h / 2),
            (x + w * 2 / 3, y + h / 2),
        ] {
            if sx < 0 || sy < 0 || sx >= SCREEN_WIDTH as i32 || sy >= SCREEN_HEIGHT as i32 {
                continue;
            }
            let idx = sy as usize * SCREEN_WIDTH + sx as usize;
            samples.push(json!({
                "xy": [sx, sy],
                "fb": rast.framebuffer[idx],
                "alpha": rast.alpha_buffer[idx],
                "depth": rast.depth_buffer[idx],
                "poly_id": rast.id_buffer[idx],
                "translucent_id": rast.translucent_id_buffer[idx],
                "edge": rast.edge_enable_buffer[idx],
            }));
        }

        json!({
            "ok": true,
            "rect": [rect.0, rect.1, rect.2, rect.3],
            "vramcnt": self.nds.shared.vram.banks.iter().map(|b| {
                json!({
                    "bank": format!("{:?}", b.id),
                    "cnt": b.cnt,
                    "target": format!("{:?}", b.target),
                })
            }).collect::<Vec<_>>(),
            "engine_a_dispcnt": self.nds.shared.engine_a.dispcnt,
            "engine_a_bgcnt": self.nds.shared.engine_a.bgcnt,
            "disp3dcnt": rast.disp3dcnt,
            "fog_color": rast.fog_color,
            "fog_offset": rast.fog_offset,
            "fog_table": rast.fog_table,
            "gxstat": gpu3d.gxstat(),
            "swap_pending": gpu3d.swap_pending,
            "geometry_locked": gpu3d.geometry_locked,
            "test_busy": gpu3d.test_busy,
            "fifo_len": gpu3d.fifo.len(),
            "fifo_empty": gpu3d.fifo.is_empty(),
            "dma9": self.nds.shared.dma9.channels.iter().enumerate().map(|(index, ch)| {
                json!({
                    "index": index,
                    "sad": ch.sad,
                    "dad": ch.dad,
                    "count": ch.count_programmed,
                    "control": ch.control,
                    "internal_sad": ch.internal_sad,
                    "internal_dad": ch.internal_dad,
                    "internal_count": ch.internal_count,
                    "active": ch.active,
                    "timing": format!("{:?}", self.nds.shared.dma9.timing(index)),
                    "word_size": self.nds.shared.dma9.word_size(index),
                    "repeat": self.nds.shared.dma9.repeat(index),
                })
            }).collect::<Vec<_>>(),
            "manual_translucent_sort": rast.manual_translucent_sort,
            "w_buffering": rast.w_buffering,
            "raster_polygons": gpu3d.raster_polygons.len(),
            "geometry_polygons": gpu3d.geometry_polygons.len(),
            "rejected_polygons": gpu3d.debug_last_rejected_polygons,
            "screen_polygon_debug": gpu3d.debug_last_screen_polygons,
            "overlap_count_limited": polys.len(),
            "overlap_polygons": polys,
            "samples": samples,
        })
    }

    fn get_2d_obj_debug(&self, req: &Value) -> Value {
        let x = req.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
        let y = req.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
        let w = req.get("w").and_then(Value::as_i64).unwrap_or(256) as i32;
        let h = req.get("h").and_then(Value::as_i64).unwrap_or(192) as i32;
        let rect = (x, y, x + w, y + h);
        let engine_b = req
            .get("engine")
            .and_then(Value::as_str)
            .is_some_and(|engine| engine.eq_ignore_ascii_case("b"));
        let oam_base = if engine_b { 0x400 } else { 0 };
        let oam = &self.nds.shared.oam[oam_base..oam_base + 0x400];
        let dispcnt = if engine_b {
            self.nds.shared.engine_b.dispcnt
        } else {
            self.nds.shared.engine_a.dispcnt
        };
        let mut sprites = Vec::new();
        for index in 0..128usize {
            let off = index * 8;
            let attr0 = u16::from_le_bytes([oam[off], oam[off + 1]]);
            let attr1 = u16::from_le_bytes([oam[off + 2], oam[off + 3]]);
            let attr2 = u16::from_le_bytes([oam[off + 4], oam[off + 5]]);
            let affine = attr0 & (1 << 8) != 0;
            let disabled_or_double = attr0 & (1 << 9) != 0;
            if !affine && disabled_or_double {
                continue;
            }
            let mut sx = (attr1 & 0x01FF) as i32;
            if sx >= 256 {
                sx -= 512;
            }
            let sy = (attr0 & 0x00FF) as i32;
            let shape = ((attr0 >> 14) & 0x3) as u8;
            let size = ((attr1 >> 14) & 0x3) as u8;
            let (obj_w, obj_h) = obj_size(shape, size);
            let box_w = if affine && disabled_or_double {
                obj_w * 2
            } else {
                obj_w
            };
            let box_h = if affine && disabled_or_double {
                obj_h * 2
            } else {
                obj_h
            };
            let bbox = (sx, sy, sx + box_w, sy + box_h);
            if bbox.2 < rect.0 || bbox.0 >= rect.2 || bbox.3 < rect.1 || bbox.1 >= rect.3 {
                continue;
            }
            sprites.push(json!({
                "index": index,
                "bbox": [bbox.0, bbox.1, bbox.2, bbox.3],
                "attr0": attr0,
                "attr1": attr1,
                "attr2": attr2,
                "affine": affine,
                "double_size": affine && disabled_or_double,
                "gfx_mode": (attr0 >> 10) & 0x3,
                "mosaic": attr0 & (1 << 12) != 0,
                "color_256": attr0 & (1 << 13) != 0,
                "shape": shape,
                "size": size,
                "priority": (attr2 >> 10) & 0x3,
                "tile": attr2 & 0x03FF,
                "palette_bank": (attr2 >> 12) & 0xF,
            }));
        }
        json!({
            "ok": true,
            "rect": [rect.0, rect.1, rect.2, rect.3],
            "engine": if engine_b { "B" } else { "A" },
            "dispcnt": dispcnt,
            "sprites": sprites,
        })
    }

    fn dump_3d_texture(&self, req: &Value) -> Result<(Value, Vec<u8>, bool), String> {
        let index = req
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| "dump_3d_texture requires polygon index".to_string())?
            as usize;
        let p = self
            .nds
            .shared
            .gpu3d
            .raster_polygons
            .get(index)
            .ok_or_else(|| format!("polygon index {index} out of range"))?;
        let tp = TexParams::from_register(p.tex_image_param);
        if tp.is_disabled() {
            return Err(format!("polygon {index} has texture disabled"));
        }

        let mut opaque = 0usize;
        let mut transparent = 0usize;
        let mut blob = Vec::with_capacity((tp.width * tp.height * 3) as usize);
        for y in 0..tp.height {
            for x in 0..tp.width {
                let texel = texture::sample(
                    tp,
                    x as i32,
                    y as i32,
                    p.palette_base,
                    &self.nds.shared.vram,
                );
                if texel.alpha == 0 {
                    transparent += 1;
                    blob.extend_from_slice(&[0, 0, 0]);
                } else {
                    opaque += 1;
                    let c = texel.color;
                    let r = c & 0x1F;
                    let g = (c >> 5) & 0x1F;
                    let b = (c >> 10) & 0x1F;
                    blob.push(((r << 3) | (r >> 2)) as u8);
                    blob.push(((g << 3) | (g >> 2)) as u8);
                    blob.push(((b << 3) | (b >> 2)) as u8);
                }
            }
        }

        Ok((
            json!({
                "ok": true,
                "index": index,
                "w": tp.width,
                "h": tp.height,
                "fmt": "RGB8",
                "tex": p.tex_image_param,
                "palette_base": p.palette_base,
                "opaque": opaque,
                "transparent": transparent,
                "vramcnt": self.nds.shared.vram.banks.iter().map(|b| {
                    json!({
                        "bank": format!("{:?}", b.id),
                        "cnt": b.cnt,
                        "target": format!("{:?}", b.target),
                    })
                }).collect::<Vec<_>>(),
            }),
            blob,
            false,
        ))
    }

    fn dump_texture_image_raw(&self, req: &Value) -> Result<(Value, Vec<u8>, bool), String> {
        let addr = req.get("addr").and_then(Value::as_u64).unwrap_or(0) as u32;
        let len = req.get("len").and_then(Value::as_u64).unwrap_or(0x200) as usize;
        let len = len.min(0x8_0000);
        let mut blob = Vec::with_capacity(len);
        for off in 0..len {
            blob.push(self.nds.shared.vram.read_texture_image(addr + off as u32));
        }
        Ok((
            json!({
                "ok": true,
                "addr": addr,
                "len": len,
            }),
            blob,
            false,
        ))
    }

    fn dump_texture_palette_raw(&self, req: &Value) -> Result<(Value, Vec<u8>, bool), String> {
        let addr = req.get("addr").and_then(Value::as_u64).unwrap_or(0) as u32;
        let len = req.get("len").and_then(Value::as_u64).unwrap_or(0x20) as usize;
        let len = len.min(0x2_0000);
        let mut blob = Vec::with_capacity(len);
        for off in 0..len {
            blob.push(self.nds.shared.vram.read_texture_palette(addr + off as u32));
        }
        Ok((
            json!({
                "ok": true,
                "addr": addr,
                "len": len,
            }),
            blob,
            false,
        ))
    }

    fn dump_main_ram_raw(&self, req: &Value) -> Result<(Value, Vec<u8>, bool), String> {
        let addr = req
            .get("addr")
            .and_then(Value::as_u64)
            .unwrap_or(0x0200_0000) as u32;
        let len = req.get("len").and_then(Value::as_u64).unwrap_or(0x200) as usize;
        let off = (addr as usize) & 0x3F_FFFF;
        if off >= self.nds.shared.main_ram.len() {
            return Err(format!("addr 0x{addr:X} is outside main RAM"));
        }
        let end = off.saturating_add(len).min(self.nds.shared.main_ram.len());
        let blob = self.nds.shared.main_ram[off..end].to_vec();
        Ok((
            json!({
                "ok": true,
                "addr": addr,
                "offset": off,
                "len": blob.len(),
            }),
            blob,
            false,
        ))
    }

    fn poke_main_ram_raw(
        &mut self,
        req: &Value,
        blob: &[u8],
    ) -> Result<(Value, Vec<u8>, bool), String> {
        let addr =
            req.get("addr")
                .and_then(Value::as_u64)
                .ok_or_else(|| "poke_main_ram_raw requires addr".to_string())? as u32;
        let off = (addr as usize) & 0x3F_FFFF;
        if off >= self.nds.shared.main_ram.len() {
            return Err(format!("addr 0x{addr:X} is outside main RAM"));
        }
        let end = off.saturating_add(blob.len());
        if end > self.nds.shared.main_ram.len() {
            return Err(format!(
                "write 0x{:X} bytes at 0x{addr:X} exceeds main RAM",
                blob.len()
            ));
        }
        self.nds.shared.main_ram[off..end].copy_from_slice(blob);
        Ok((
            json!({
                "ok": true,
                "addr": addr,
                "offset": off,
                "len": blob.len(),
            }),
            Vec::new(),
            false,
        ))
    }

    fn dump_vram_bank_raw(&self, req: &Value) -> Result<(Value, Vec<u8>, bool), String> {
        let bank_id = parse_bank_id(req.get("bank").and_then(Value::as_str).unwrap_or("A"))?;
        let addr = req.get("addr").and_then(Value::as_u64).unwrap_or(0) as usize;
        let bank = &self.nds.shared.vram.banks[bank_id as usize];
        if addr >= bank.data.len() {
            return Err(format!(
                "addr 0x{addr:X} is outside VRAM bank {:?} size 0x{:X}",
                bank.id,
                bank.data.len()
            ));
        }
        let len = req.get("len").and_then(Value::as_u64).unwrap_or(0x200) as usize;
        let end = addr.saturating_add(len).min(bank.data.len());
        let blob = bank.data[addr..end].to_vec();
        Ok((
            json!({
                "ok": true,
                "bank": format!("{:?}", bank.id),
                "cnt": bank.cnt,
                "target": format!("{:?}", bank.target),
                "addr": addr,
                "len": blob.len(),
            }),
            blob,
            false,
        ))
    }

    fn rerender_3d_debug(&mut self, req: &Value) -> Result<(), String> {
        let disable_textures = req
            .get("disable_textures")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let disable_alpha_test = req
            .get("disable_alpha_test")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let disable_fog = req
            .get("disable_fog")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let disable_edge_marking = req
            .get("disable_edge_marking")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let index_colors = req
            .get("index_colors")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut polygons = self.nds.shared.gpu3d.raster_polygons.clone();
        if index_colors {
            for (index, p) in polygons.iter_mut().enumerate() {
                let r = ((index * 7 + 3) & 0x1F) as u16;
                let g = ((index * 13 + 9) & 0x1F) as u16;
                let b = ((index * 19 + 17) & 0x1F) as u16;
                let color = r | (g << 5) | (b << 10);
                p.attr = (p.attr & !(0x1F << 16)) | (31 << 16) | (1 << 6) | (1 << 7);
                p.tex_image_param = 0;
                for v in &mut p.vertices {
                    v.color = color;
                }
            }
        }
        if let Some(overrides) = req.get("palette_overrides").and_then(Value::as_array) {
            for entry in overrides {
                let Some(index) = entry.get("index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(palette_base) = entry.get("palette_base").and_then(Value::as_u64) else {
                    continue;
                };
                if let Some(polygon) = polygons.get_mut(index as usize) {
                    polygon.palette_base = palette_base as u16;
                }
            }
        }
        if let Some(overrides) = req.get("texture_param_overrides").and_then(Value::as_array) {
            for entry in overrides {
                let Some(index) = entry.get("index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(tex_image_param) = entry.get("tex").and_then(Value::as_u64) else {
                    continue;
                };
                if let Some(polygon) = polygons.get_mut(index as usize) {
                    polygon.tex_image_param = tex_image_param as u32;
                }
            }
        }
        if let Some(skip_indices) = req.get("skip_indices").and_then(Value::as_array) {
            let mut skip = Vec::new();
            for value in skip_indices {
                if let Some(index) = value.as_u64() {
                    skip.push(index as usize);
                }
            }
            polygons = polygons
                .into_iter()
                .enumerate()
                .filter_map(|(index, polygon)| (!skip.contains(&index)).then_some(polygon))
                .collect();
        }
        let mut vram = self.nds.shared.vram.clone();
        if let Some(uploads) = req.get("texture_uploads").and_then(Value::as_array) {
            for upload in uploads {
                let Some(src) = upload.get("src").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(dst) = upload.get("dst").and_then(Value::as_u64) else {
                    continue;
                };
                let len = upload.get("len").and_then(Value::as_u64).unwrap_or(0) as usize;
                let src_off = (src as usize) & 0x3F_FFFF;
                for i in 0..len {
                    if let Some(&byte) = self.nds.shared.main_ram.get(src_off + i) {
                        write_texture_image_debug(&mut vram, dst as u32 + i as u32, byte);
                    }
                }
            }
        }
        if let Some(uploads) = req.get("palette_uploads").and_then(Value::as_array) {
            for upload in uploads {
                let Some(src) = upload.get("src").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(dst) = upload.get("dst").and_then(Value::as_u64) else {
                    continue;
                };
                let len = upload.get("len").and_then(Value::as_u64).unwrap_or(0) as usize;
                let src_off = (src as usize) & 0x3F_FFFF;
                for i in 0..len {
                    if let Some(&byte) = self.nds.shared.main_ram.get(src_off + i) {
                        write_texture_palette_debug(&mut vram, dst as u32 + i as u32, byte);
                    }
                }
            }
        }
        let rast = &mut self.nds.shared.gpu3d.rasterizer;
        let old_disp3dcnt = rast.disp3dcnt;
        if disable_textures {
            rast.disp3dcnt &= !(1 << 0);
        }
        if disable_alpha_test {
            rast.disp3dcnt &= !(1 << 2);
        }
        if disable_fog {
            rast.disp3dcnt &= !(1 << 7);
        }
        if disable_edge_marking {
            rast.disp3dcnt &= !(1 << 5);
        }
        rast.render_frame(&polygons, Some(&vram));
        rast.disp3dcnt = old_disp3dcnt;
        Ok(())
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

fn write_texture_image_debug(vram: &mut VramRouter, addr: u32, val: u8) {
    let addr = addr & 0x7_FFFF;
    for bank in &mut vram.banks {
        if let VramTarget::TextureImage { slot } = bank.target {
            let base = (slot as u32) * 0x2_0000;
            let span = bank.id.size() as u32;
            if addr >= base && addr < base + span {
                bank.data[(addr - base) as usize] = val;
            }
        }
    }
}

fn write_texture_palette_debug(vram: &mut VramRouter, addr: u32, val: u8) {
    let addr = addr & 0x1_FFFF;
    for bank in &mut vram.banks {
        if let VramTarget::TexturePalette { slot } = bank.target {
            let base = (slot as u32) * 0x4000;
            let span = bank.id.size() as u32;
            if addr >= base && addr < base + span {
                bank.data[(addr - base) as usize] = val;
            }
        }
    }
}

fn parse_bank_id(s: &str) -> Result<BankId, String> {
    match s.trim().to_ascii_uppercase().as_str() {
        "A" => Ok(BankId::A),
        "B" => Ok(BankId::B),
        "C" => Ok(BankId::C),
        "D" => Ok(BankId::D),
        "E" => Ok(BankId::E),
        "F" => Ok(BankId::F),
        "G" => Ok(BankId::G),
        "H" => Ok(BankId::H),
        "I" => Ok(BankId::I),
        _ => Err(format!("unknown VRAM bank {s:?}")),
    }
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

fn obj_size(shape: u8, size: u8) -> (i32, i32) {
    match (shape & 0x3, size & 0x3) {
        (0, 0) => (8, 8),
        (0, 1) => (16, 16),
        (0, 2) => (32, 32),
        (0, 3) => (64, 64),
        (1, 0) => (16, 8),
        (1, 1) => (32, 8),
        (1, 2) => (32, 16),
        (1, 3) => (64, 32),
        (2, 0) => (8, 16),
        (2, 1) => (8, 32),
        (2, 2) => (16, 32),
        (2, 3) => (32, 64),
        _ => (0, 0),
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

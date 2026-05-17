//! 2D graphics engines (Engine A + Engine B).
//!
//! Phase 3 covers:
//! - DISPCNT register family + BG control / scroll / affine.
//! - Text BG rendering (modes 0-2 baseline) + 256-color affine BGs.
//! - OBJ rendering (regular + affine, 1D mapping with configurable
//!   boundary; 2D mapping kept for completeness).
//! - Window-priority compositing + alpha/brightness blends + MASTER_BRIGHT.
//!
//! Out of scope until later phases: extended palette, extended affine
//! bitmap modes (3-5), large-screen mode 6, display capture, the 3D layer
//! source. These are stubbed to render transparent.

pub mod bg;
pub mod obj;
pub mod compositor;

use serde::{Deserialize, Serialize};

/// Which engine an instance represents — Engine A is the full-feature one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Which {
    A,
    B,
}

/// Engine A or B register set + per-frame internal state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Engine2d {
    pub which: Which,

    pub dispcnt: u32,
    pub bgcnt: [u16; 4],
    pub bg_hofs: [u16; 4],
    pub bg_vofs: [u16; 4],

    /// BG2/3 affine parameters (PA, PB, PC, PD), 8.8 fixed-point per component.
    pub bg2_pa: i16, pub bg2_pb: i16, pub bg2_pc: i16, pub bg2_pd: i16,
    pub bg3_pa: i16, pub bg3_pb: i16, pub bg3_pc: i16, pub bg3_pd: i16,
    /// Reference points (latched at frame start), 28-bit signed 8.8.
    pub bg2_x_latch: i32, pub bg2_y_latch: i32,
    pub bg3_x_latch: i32, pub bg3_y_latch: i32,
    /// Internal scanline-advanced reference points.
    pub bg2_x_int: i32, pub bg2_y_int: i32,
    pub bg3_x_int: i32, pub bg3_y_int: i32,

    pub win0h: u16, pub win1h: u16,
    pub win0v: u16, pub win1v: u16,
    pub winin: u16, pub winout: u16,

    pub mosaic: u16,
    pub bldcnt: u16, pub bldalpha: u16, pub bldy: u16,

    pub master_bright: u16,
}

impl Engine2d {
    pub fn new(which: Which) -> Self {
        Engine2d {
            which,
            dispcnt: 0,
            bgcnt: [0; 4],
            bg_hofs: [0; 4],
            bg_vofs: [0; 4],
            bg2_pa: 0x100, bg2_pb: 0, bg2_pc: 0, bg2_pd: 0x100,
            bg3_pa: 0x100, bg3_pb: 0, bg3_pc: 0, bg3_pd: 0x100,
            bg2_x_latch: 0, bg2_y_latch: 0,
            bg3_x_latch: 0, bg3_y_latch: 0,
            bg2_x_int: 0, bg2_y_int: 0,
            bg3_x_int: 0, bg3_y_int: 0,
            win0h: 0, win1h: 0, win0v: 0, win1v: 0,
            winin: 0, winout: 0,
            mosaic: 0,
            bldcnt: 0, bldalpha: 0, bldy: 0,
            master_bright: 0,
        }
    }

    /// Latch affine reference points at the start of a frame (line 0).
    pub fn latch_affine_refs(&mut self) {
        self.bg2_x_int = self.bg2_x_latch;
        self.bg2_y_int = self.bg2_y_latch;
        self.bg3_x_int = self.bg3_x_latch;
        self.bg3_y_int = self.bg3_y_latch;
    }

    /// Per-scanline advance: reference += (PB, PD).
    pub fn advance_affine_refs(&mut self) {
        self.bg2_x_int = self.bg2_x_int.wrapping_add(self.bg2_pb as i32);
        self.bg2_y_int = self.bg2_y_int.wrapping_add(self.bg2_pd as i32);
        self.bg3_x_int = self.bg3_x_int.wrapping_add(self.bg3_pb as i32);
        self.bg3_y_int = self.bg3_y_int.wrapping_add(self.bg3_pd as i32);
    }

    /// Whether this engine has a 3D layer slot (Engine A only). Phase 3
    /// renders this layer as transparent — wired in Phase 6/7.
    pub fn has_3d_layer(&self) -> bool {
        self.which == Which::A
    }
}

/// Result of compositing one pixel — emitted by the compositor and consumed
/// when writing into the framebuffer.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedPixel {
    pub color: u16,
}

/// Render one scanline. `palette` and `oam` are this engine's 1 KB halves;
/// `vram` is the global router (the engine reads via its target views).
/// `framebuffer` is 256 × 192 u16, indexed by `line * 256 + x`.
/// `framebuffer_3d` is the 3D rasterizer's output, consulted when
/// `DISPCNT` bit 3 selects "BG0 source is 3D" — Engine A only. Pass `None`
/// for Engine B or when 3D isn't being composited.
pub fn render_scanline(
    engine: &mut Engine2d,
    line: u16,
    palette: &[u8],
    oam: &[u8],
    vram: &crate::vram::VramRouter,
    framebuffer: &mut [u16],
    framebuffer_3d: Option<&[u16]>,
) {
    let mode = engine.dispcnt & 0x07;

    // Display mode (DISPCNT bits 16-17, ARM9-ish; only Engine A reads these).
    let display_mode = (engine.dispcnt >> 16) & 0x3;
    let row_start = line as usize * 256;

    // Display Off: white.
    if engine.which == Which::A && display_mode == 0 {
        for x in 0..256 { framebuffer[row_start + x] = 0x7FFF; }
        return;
    }

    // Direct VRAM display (mode 2) and Main Memory display (mode 3) are
    // wired in later phases; for now fall through to normal compositing.

    // Collect BG layers active in this mode.
    let (text_bgs, affine_bgs): (&[usize], &[usize]) = match mode {
        0 => (&[0, 1, 2, 3][..], &[][..]),
        1 => (&[0, 1, 2][..],    &[3][..]),
        2 => (&[0, 1][..],       &[2, 3][..]),
        3 => (&[0, 1, 2][..],    &[3][..]),
        4 => (&[0, 1][..],       &[2, 3][..]),
        5 => (&[0, 1][..],       &[2, 3][..]),
        _ => (&[][..], &[][..]),
    };

    let dispcnt_bg_enable = ((engine.dispcnt >> 8) & 0xF) as u8;

    let mut bg_layers: [Option<[Option<bg::BgPixel>; 256]>; 4] = [None, None, None, None];

    // BG0 = 3D path: DISPCNT bit 3 says "BG0 source is the 3D framebuffer."
    // Engine A only — `framebuffer_3d` is None on Engine B.
    let bg0_is_3d = engine.dispcnt & (1 << 3) != 0 && framebuffer_3d.is_some();

    for &n in text_bgs {
        if dispcnt_bg_enable & (1 << n) == 0 { continue; }
        if n == 0 && bg0_is_3d {
            // Synthesize BG0 from the 3D framebuffer.
            let fb3d = framebuffer_3d.unwrap();
            let line_off = line as usize * 256;
            let priority = (engine.bgcnt[0] & 0x3) as u8;
            let mut layer = [None; 256];
            for x in 0..256 {
                let pixel = fb3d[line_off + x];
                if pixel & (1 << 15) != 0 {
                    layer[x] = Some(bg::BgPixel { color: pixel & 0x7FFF, priority, bg_index: 0 });
                }
            }
            bg_layers[0] = Some(layer);
            continue;
        }
        let mut layer = [None; 256];
        bg::render_text_bg(engine, n, line, palette, vram, &mut layer);
        bg_layers[n] = Some(layer);
    }
    for &n in affine_bgs {
        if dispcnt_bg_enable & (1 << n) == 0 { continue; }
        let mut layer = [None; 256];
        bg::render_affine_bg(engine, n, palette, vram, &mut layer);
        bg_layers[n] = Some(layer);
    }

    // OBJ pass.
    let mut obj_line = obj::ObjLine::default();
    obj::render_objs(engine, line, palette, oam, vram, &mut obj_line);

    // Composite.
    compositor::compose_scanline(engine, line, palette, &bg_layers, &obj_line, framebuffer);

    if mode == 1 || mode == 2 {
        engine.advance_affine_refs();
    }
}

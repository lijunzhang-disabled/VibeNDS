//! Per-scanline compositor: gathers BG and OBJ candidates, applies window
//! masking, picks top + second pixel by priority, applies alpha/brightness
//! blending, then MASTER_BRIGHT.

use super::bg::BgPixel;
use super::obj::ObjLine;
use super::Engine2d;

const SCREEN_WIDTH: usize = 256;

/// Per-pixel window-derived flags.
#[derive(Default, Clone, Copy)]
struct WinFlags {
    bg_enable: [bool; 4],
    obj_enable: bool,
    effects_enable: bool,
}

/// Layer label for blend target selection. Matches BLDCNT bit positions:
/// 0..3 = BG0..BG3, 4 = OBJ, 5 = backdrop.
const LAYER_BG0: u8 = 0;
const LAYER_BG1: u8 = 1;
const LAYER_BG2: u8 = 2;
const LAYER_BG3: u8 = 3;
const LAYER_OBJ: u8 = 4;
const LAYER_BACKDROP: u8 = 5;

#[derive(Clone, Copy)]
struct PixelCandidate {
    color: u16,
    priority: u8,
    layer: u8,
    /// Semi-transparent OBJ marker — forces alpha blend regardless of BLDCNT.
    semi_transparent: bool,
}

/// Compose one scanline into `framebuffer[line*256..line*256+256]`.
pub fn compose_scanline(
    engine: &Engine2d,
    line: u16,
    palette: &[u8],
    bg_layers: &[Option<[Option<BgPixel>; SCREEN_WIDTH]>; 4],
    obj_line: &ObjLine,
    framebuffer: &mut [u16],
) {
    let row_start = line as usize * SCREEN_WIDTH;

    // Forced blank (DISPCNT bit 7) — output white.
    if engine.dispcnt & (1 << 7) != 0 {
        for x in 0..SCREEN_WIDTH {
            framebuffer[row_start + x] = 0x7FFF;
        }
        return;
    }

    let backdrop = u16::from_le_bytes([palette[0], palette[1]]);

    let win_flags = compute_window_flags(engine, line, obj_line);

    for x in 0..SCREEN_WIDTH {
        let win = win_flags[x];

        // Collect candidates into a small inline buffer (max 5: 4 BGs + OBJ).
        let mut buf: [Option<PixelCandidate>; 5] = [None; 5];
        let mut len = 0usize;
        for n in 0..4 {
            if !win.bg_enable[n] { continue; }
            if let Some(layers) = &bg_layers[n] {
                if let Some(p) = layers[x] {
                    buf[len] = Some(PixelCandidate {
                        color: p.color, priority: p.priority,
                        layer: p.bg_index, semi_transparent: false,
                    });
                    len += 1;
                }
            }
        }
        if win.obj_enable {
            if let Some(o) = obj_line.pixel[x] {
                buf[len] = Some(PixelCandidate {
                    color: o.color, priority: o.priority,
                    layer: LAYER_OBJ,
                    semi_transparent: o.gfx_mode == 1,
                });
                len += 1;
            }
        }

        // Sort active candidates in-place by priority asc, ties by sublayer rank.
        buf[..len].sort_by(|a, b| {
            let a = a.unwrap(); let b = b.unwrap();
            a.priority.cmp(&b.priority)
                .then_with(|| sublayer_rank(a.layer).cmp(&sublayer_rank(b.layer)))
        });

        let backdrop_cand = PixelCandidate {
            color: backdrop, priority: 4, layer: LAYER_BACKDROP, semi_transparent: false,
        };
        let (top, second) = match len {
            0 => (backdrop_cand, backdrop_cand),
            1 => (buf[0].unwrap(), backdrop_cand),
            _ => (buf[0].unwrap(), buf[1].unwrap()),
        };

        let final_color = if win.effects_enable {
            apply_blend(engine, top, second)
        } else if top.semi_transparent && is_second_target(engine, second.layer) {
            // Semi-transparent OBJ always blends with a valid 2nd target.
            alpha_blend(top.color, second.color, engine.bldalpha)
        } else {
            top.color
        };

        framebuffer[row_start + x] = apply_master_bright(final_color, engine.master_bright);
    }
}

/// Lower number = renders on top within the same priority bucket.
fn sublayer_rank(layer: u8) -> u8 {
    match layer {
        LAYER_OBJ => 0, // OBJ beats BG at equal priority
        LAYER_BG0 => 1, LAYER_BG1 => 2, LAYER_BG2 => 3, LAYER_BG3 => 4,
        _ => 5,
    }
}

fn compute_window_flags(engine: &Engine2d, line: u16, obj_line: &ObjLine)
    -> [WinFlags; SCREEN_WIDTH]
{
    let win0_enable = engine.dispcnt & (1 << 13) != 0;
    let win1_enable = engine.dispcnt & (1 << 14) != 0;
    let objwin_enable = engine.dispcnt & (1 << 15) != 0;

    if !win0_enable && !win1_enable && !objwin_enable {
        // Fast path: all layers visible, effects enabled.
        let everything = WinFlags {
            bg_enable: [true; 4], obj_enable: true, effects_enable: true,
        };
        return [everything; SCREEN_WIDTH];
    }

    let outside = WinFlags {
        bg_enable: [
            engine.winout & (1 << 0) != 0,
            engine.winout & (1 << 1) != 0,
            engine.winout & (1 << 2) != 0,
            engine.winout & (1 << 3) != 0,
        ],
        obj_enable: engine.winout & (1 << 4) != 0,
        effects_enable: engine.winout & (1 << 5) != 0,
    };
    let win0_flags = win_flags_from_bits(engine.winin & 0xFF);
    let win1_flags = win_flags_from_bits(engine.winin >> 8);
    let objwin_flags = win_flags_from_bits(engine.winout >> 8);

    let mut out = [outside; SCREEN_WIDTH];

    if win0_enable {
        let (h_lo, h_hi) = ((engine.win0h >> 8) as i32, (engine.win0h & 0xFF) as i32);
        let (v_lo, v_hi) = ((engine.win0v >> 8) as i32, (engine.win0v & 0xFF) as i32);
        if line_in_range(line, v_lo, v_hi) {
            for x in 0..SCREEN_WIDTH as i32 {
                if x_in_range(x, h_lo, h_hi) {
                    out[x as usize] = win0_flags;
                }
            }
        }
    }
    if win1_enable {
        let (h_lo, h_hi) = ((engine.win1h >> 8) as i32, (engine.win1h & 0xFF) as i32);
        let (v_lo, v_hi) = ((engine.win1v >> 8) as i32, (engine.win1v & 0xFF) as i32);
        if line_in_range(line, v_lo, v_hi) {
            for x in 0..SCREEN_WIDTH as i32 {
                if x_in_range(x, h_lo, h_hi) && out[x as usize].matches_default(&outside) {
                    out[x as usize] = win1_flags;
                }
            }
        }
    }
    if objwin_enable {
        for x in 0..SCREEN_WIDTH {
            if obj_line.window[x] && out[x].matches_default(&outside) {
                out[x] = objwin_flags;
            }
        }
    }

    out
}

impl WinFlags {
    fn matches_default(&self, def: &WinFlags) -> bool {
        self.bg_enable == def.bg_enable
            && self.obj_enable == def.obj_enable
            && self.effects_enable == def.effects_enable
    }
}

fn win_flags_from_bits(bits: u16) -> WinFlags {
    WinFlags {
        bg_enable: [
            bits & (1 << 0) != 0,
            bits & (1 << 1) != 0,
            bits & (1 << 2) != 0,
            bits & (1 << 3) != 0,
        ],
        obj_enable: bits & (1 << 4) != 0,
        effects_enable: bits & (1 << 5) != 0,
    }
}

fn line_in_range(line: u16, lo: i32, hi: i32) -> bool {
    let line = line as i32;
    if lo <= hi { (lo..hi).contains(&line) }
    else        { line >= lo || line < hi }
}

fn x_in_range(x: i32, lo: i32, hi: i32) -> bool {
    if lo <= hi { (lo..hi).contains(&x) }
    else        { x >= lo || x < hi }
}

fn is_first_target(engine: &Engine2d, layer: u8) -> bool {
    engine.bldcnt & (1 << layer as u16) != 0
}

fn is_second_target(engine: &Engine2d, layer: u8) -> bool {
    engine.bldcnt & (1 << (layer as u16 + 8)) != 0
}

fn apply_blend(engine: &Engine2d, top: PixelCandidate, second: PixelCandidate) -> u16 {
    // Semi-transparent OBJ always alpha-blends with a valid 2nd target,
    // bypassing BLDCNT's first-target check.
    if top.semi_transparent && is_second_target(engine, second.layer) {
        return alpha_blend(top.color, second.color, engine.bldalpha);
    }

    let mode = (engine.bldcnt >> 6) & 0x3;
    if !is_first_target(engine, top.layer) {
        return top.color;
    }
    match mode {
        1 if is_second_target(engine, second.layer) => alpha_blend(top.color, second.color, engine.bldalpha),
        2 => brightness_increase(top.color, engine.bldy),
        3 => brightness_decrease(top.color, engine.bldy),
        _ => top.color,
    }
}

fn alpha_blend(top: u16, bot: u16, bldalpha: u16) -> u16 {
    let eva = (bldalpha & 0x1F).min(16) as u32;
    let evb = ((bldalpha >> 8) & 0x1F).min(16) as u32;
    let blend_chan = |t: u32, b: u32| -> u16 {
        ((t * eva + b * evb) / 16).min(31) as u16
    };
    let tr = (top & 0x1F) as u32;
    let tg = ((top >> 5) & 0x1F) as u32;
    let tb = ((top >> 10) & 0x1F) as u32;
    let br = (bot & 0x1F) as u32;
    let bg = ((bot >> 5) & 0x1F) as u32;
    let bb = ((bot >> 10) & 0x1F) as u32;
    blend_chan(tr, br) | (blend_chan(tg, bg) << 5) | (blend_chan(tb, bb) << 10)
}

fn brightness_increase(color: u16, bldy: u16) -> u16 {
    let evy = (bldy & 0x1F).min(16) as u32;
    let chan = |c: u32| -> u16 { (c + ((31 - c) * evy) / 16) as u16 };
    let r = (color & 0x1F) as u32;
    let g = ((color >> 5) & 0x1F) as u32;
    let b = ((color >> 10) & 0x1F) as u32;
    chan(r) | (chan(g) << 5) | (chan(b) << 10)
}

fn brightness_decrease(color: u16, bldy: u16) -> u16 {
    let evy = (bldy & 0x1F).min(16) as u32;
    let chan = |c: u32| -> u16 { (c - (c * evy) / 16) as u16 };
    let r = (color & 0x1F) as u32;
    let g = ((color >> 5) & 0x1F) as u32;
    let b = ((color >> 10) & 0x1F) as u32;
    chan(r) | (chan(g) << 5) | (chan(b) << 10)
}

fn apply_master_bright(color: u16, master_bright: u16) -> u16 {
    let mode = (master_bright >> 14) & 0x3;
    let factor = (master_bright & 0x1F).min(16) as u32;
    match mode {
        0 => color,
        1 => brightness_increase(color, factor as u16),
        2 => brightness_decrease(color, factor as u16),
        _ => color,
    }
}

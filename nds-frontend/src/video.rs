//! Dual-screen video output.
//!
//! Single SDL2 window stacking the two 256x192 screens vertically.

use nds_core::{SCREEN_HEIGHT, SCREEN_WIDTH};
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::{Canvas, TextureCreator};
use sdl2::video::{Window, WindowContext};

/// Default vertical gap between the two screens, measured in NDS pixels.
///
/// A physical DS-style gap is roughly 90 native pixels, but that is bulky on a
/// desktop monitor at 2x/3x scale. Keep the default tight and let callers opt
/// into a larger bezel with `--screen-gap`.
pub const DEFAULT_SCREEN_GAP: u32 = 8;

pub struct DualScreen {
    canvas: Canvas<Window>,
    texture_creator: TextureCreator<WindowContext>,
    pixel_buffer: Vec<u8>,
    scale: u32,
    screen_gap: u32,
}

impl DualScreen {
    pub fn new(sdl: &sdl2::Sdl, scale: u32, screen_gap: u32) -> Self {
        let video = sdl.video().expect("SDL2 video");

        let total_h = (SCREEN_HEIGHT as u32) * 2 + screen_gap;
        let window = video
            .window("NDS Emulator", SCREEN_WIDTH as u32 * scale, total_h * scale)
            .position_centered()
            .build()
            .expect("create window");

        let mut canvas = window
            .into_canvas()
            .software()
            .build()
            .expect("create canvas");
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();
        canvas.present();

        let texture_creator = canvas.texture_creator();
        let pixel_buffer = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];

        let info = canvas.info();
        eprintln!("SDL2 renderer: {} (flags: 0x{:X})", info.name, info.flags);

        DualScreen {
            canvas,
            texture_creator,
            pixel_buffer,
            scale,
            screen_gap,
        }
    }

    /// Convert a single 256x192 BGR555 framebuffer to ARGB8888 in-place.
    fn convert(&mut self, src: &[u16]) {
        for i in 0..(SCREEN_WIDTH * SCREEN_HEIGHT) {
            let c = src[i];
            let r = ((c & 0x1F) as u8) << 3;
            let g = (((c >> 5) & 0x1F) as u8) << 3;
            let b = (((c >> 10) & 0x1F) as u8) << 3;
            let off = i * 4;
            self.pixel_buffer[off] = b;
            self.pixel_buffer[off + 1] = g;
            self.pixel_buffer[off + 2] = r;
            self.pixel_buffer[off + 3] = 0xFF;
        }
    }

    fn blit_one(&mut self, src: &[u16], dst: Rect) {
        self.convert(src);
        let mut tex = self
            .texture_creator
            .create_texture_streaming(
                PixelFormatEnum::ARGB8888,
                SCREEN_WIDTH as u32,
                SCREEN_HEIGHT as u32,
            )
            .expect("create texture");
        tex.update(None, &self.pixel_buffer, SCREEN_WIDTH * 4)
            .expect("update");
        self.canvas.copy(&tex, None, Some(dst)).expect("copy");
    }

    pub fn present(&mut self, top: &[u16], bot: &[u16]) {
        self.canvas.set_draw_color(Color::RGB(0, 0, 0));
        self.canvas.clear();

        let dst_top = Rect::new(
            0,
            0,
            SCREEN_WIDTH as u32 * self.scale,
            SCREEN_HEIGHT as u32 * self.scale,
        );
        let dst_bot = Rect::new(
            0,
            ((SCREEN_HEIGHT as u32 + self.screen_gap) * self.scale) as i32,
            SCREEN_WIDTH as u32 * self.scale,
            SCREEN_HEIGHT as u32 * self.scale,
        );

        self.blit_one(top, dst_top);
        self.blit_one(bot, dst_bot);

        self.canvas.present();
    }
}

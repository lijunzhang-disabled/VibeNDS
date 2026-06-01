//! NDS emulator core. See `../PLAN.md` and `../ARCHITECTURE.md`.

pub mod audio;
pub mod bios;
pub mod bus;
pub mod cart;
pub mod cpu;
pub mod dma;
pub mod gpu2d;
pub mod gpu3d;
pub mod interrupt;
pub mod ipc;
pub mod scheduler;
pub mod spi;
pub mod timer;
pub mod vram;

pub use bus::{Arm7Memory, Arm9Memory, Bus7, Bus9, SharedState};
pub use cart::{Cart, CartHeader};
pub use cpu::bus::CpuBus;
pub use cpu::{Cpu, CpuMode, Psr};
pub use gpu2d::{Engine2d, Which as EngineWhich};
pub use interrupt::{InterruptController, Irq};
pub use scheduler::{CpuId, Event, EventKind, Scheduler};

/// Timing constants in the ARM7 clock domain (1 ARM7 cycle = 2 ARM9 cycles).
pub const ARM7_CLOCK_HZ: u32 = 33_513_982;
pub const ARM9_CLOCK_HZ: u32 = 67_027_964;
pub const CYCLES_PER_DOT_ARM7: u32 = 6;
pub const DOTS_PER_LINE: u32 = 355;
pub const VISIBLE_DOTS: u32 = 256;
pub const HBLANK_DOTS: u32 = DOTS_PER_LINE - VISIBLE_DOTS; // 99
pub const CYCLES_PER_LINE_ARM7: u32 = DOTS_PER_LINE * CYCLES_PER_DOT_ARM7; // 2130
pub const HDRAW_CYCLES_ARM7: u32 = VISIBLE_DOTS * CYCLES_PER_DOT_ARM7; // 1536
pub const HBLANK_CYCLES_ARM7: u32 = HBLANK_DOTS * CYCLES_PER_DOT_ARM7; // 594
pub const VISIBLE_LINES: u16 = 192;
pub const VBLANK_LINES: u16 = 71;
pub const LINES_PER_FRAME: u16 = VISIBLE_LINES + VBLANK_LINES; // 263
pub const CYCLES_PER_FRAME_ARM7: u64 = CYCLES_PER_LINE_ARM7 as u64 * LINES_PER_FRAME as u64;

pub const SCREEN_WIDTH: usize = 256;
pub const SCREEN_HEIGHT: usize = 192;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nds {
    pub cpu9: Cpu,
    pub cpu7: Cpu,
    pub mem9: Arm9Memory,
    pub mem7: Arm7Memory,
    pub shared: SharedState,
    pub scheduler: Scheduler,
    pub cart: Cart,

    pub framebuffer_top: Vec<u16>,
    pub framebuffer_bot: Vec<u16>,

    /// True when the loaded ROM is being run via direct boot (no real BIOS).
    /// Drives whether SWIs go through HLE or jump to the BIOS vector.
    pub direct_boot: bool,
}

impl Nds {
    pub fn new(bios9: Option<Vec<u8>>, bios7: Option<Vec<u8>>) -> Self {
        let mut nds = Nds {
            cpu9: Cpu::new_arm9(),
            cpu7: Cpu::new_arm7(),
            mem9: Arm9Memory::new(bios9),
            mem7: Arm7Memory::new(bios7),
            shared: SharedState::new(),
            scheduler: Scheduler::new(),
            cart: Cart::empty(),
            framebuffer_top: vec![0u16; SCREEN_WIDTH * SCREEN_HEIGHT],
            framebuffer_bot: vec![0u16; SCREEN_WIDTH * SCREEN_HEIGHT],
            direct_boot: false,
        };
        nds.schedule_initial_events();
        nds
    }

    fn schedule_initial_events(&mut self) {
        // First HBlank fires at the end of dot 256 of line 0.
        self.scheduler.schedule(Event {
            fire_time: HDRAW_CYCLES_ARM7 as u64,
            kind: EventKind::HBlank,
        });
    }

    /// Load a ROM and apply the direct-boot path. Replaces any previous cart.
    pub fn load_cart_direct_boot(&mut self, rom: Vec<u8>) -> Result<(), CartLoadError> {
        let cart = Cart::from_rom(rom).map_err(CartLoadError::Header)?;
        let header = cart.header.as_ref().expect("just parsed").clone();
        let rom_bytes = cart.rom.as_ref().expect("just parsed").clone();
        cart::direct_boot::apply(
            &mut self.cpu9,
            &mut self.cpu7,
            &mut self.mem7,
            &mut self.shared,
            &header,
            &rom_bytes,
        )
        .map_err(CartLoadError::DirectBoot)?;
        self.shared.slot1_rom = rom_bytes;
        self.shared.slot1_romctrl = 0;
        self.shared.slot1_command = [0; 8];
        self.shared.slot1_data.clear();
        self.cart = cart;
        self.direct_boot = true;
        Ok(())
    }

    pub fn run_cycles(&mut self, arm7_cycles: u64) {
        let target = self.scheduler.timestamp().saturating_add(arm7_cycles);

        while self.scheduler.timestamp() < target {
            // Lockstep: 2 ARM9 instructions per ARM7 instruction. ARM9 advances
            // first so ARM7 sees any IPC writes ARM9 made this iteration.
            let mut arm9_cycles_total = 0u32;
            for _ in 0..2 {
                if !self.cpu9.halted {
                    self.ack_direct_boot_arm9_irq_without_handler();
                    let itcm = self.cpu9.cp15.itcm;
                    let dtcm = self.cpu9.cp15.dtcm;
                    let mut bus = Bus9::new(&mut self.mem9, &mut self.shared, itcm, dtcm);
                    arm9_cycles_total += self.cpu9.step(&mut bus);
                } else {
                    arm9_cycles_total += 1;
                }
                if let Some(swi) = self.cpu9.pending_swi.take() {
                    self.handle_swi9(swi);
                }
            }

            let arm7_consumed = if !self.cpu7.halted {
                let mut bus = Bus7::new(&mut self.mem7, &mut self.shared);
                let cycles = self.cpu7.step(&mut bus) as u32;
                if let Some(swi) = self.cpu7.pending_swi.take() {
                    self.handle_swi7(swi);
                }
                if self.shared.halt7_requested {
                    self.cpu7.halted = true;
                    self.shared.halt7_requested = false;
                }
                cycles
            } else {
                1
            };

            // Timers tick in their own clock domain. ARM9 timers run at
            // the ARM9 clock; ARM7 timers at the ARM7 clock.
            self.tick_timers(arm9_cycles_total, arm7_consumed);

            // Audio mixer ticks in the ARM7 clock domain. Disjoint-borrow
            // the audio and main_ram fields so the bus_read8 closure can
            // pull sample data while the mixer mutates channel state.
            let SharedState {
                audio, main_ram, ..
            } = &mut self.shared;
            let main_ram_slice = &main_ram[..];
            audio::mixer::tick(audio, arm7_consumed, &mut |addr| {
                main_ram_slice[(addr as usize) & 0x3F_FFFF]
            });

            self.scheduler.add_cycles(arm7_consumed as u64);

            while let Some(event) = self.scheduler.pop_if_ready() {
                self.dispatch_event(event);
            }
            self.refresh_level_irqs();

            // Halt-wake: a halted CPU is skipped by the run loop above,
            // so `step()` (the usual place that clears `halted`) never
            // runs. Real ARM7TDMI / ARM946E-S wakes from
            // SWI Halt / IntrWait / VBlankIntrWait as soon as
            // `(IE & IF) != 0` — independent of IME and CPSR.I. We
            // mirror that here: after each chunk + event dispatch, clear
            // `halted` on either CPU if its controller has an unmasked
            // IRQ. The next iteration's step() then delivers the IRQ
            // through the normal path. See
            // debug/2026-05-08_halt-wake-inherited.md.
            //
            // IntrWait gate: if `intrwait_mask != 0`, the CPU is parked
            // by SWI 0x04 / 0x05 and only an IRQ whose bit is in the
            // mask should wake it. Any other IRQ keeps it halted (real
            // BIOS re-enters HALT in its loop). Mask is cleared on wake.
            // See debug/2026-05-29_intrwait-mask-inherited.md.
            if self.cpu9.halted {
                let wake = if self.cpu9.intrwait_mask != 0 {
                    self.shared.irq9.has_matching_irq(self.cpu9.intrwait_mask)
                } else {
                    self.shared.irq9.has_unmasked_irq()
                };
                if wake {
                    self.cpu9.halted = false;
                    self.cpu9.intrwait_mask = 0;
                }
            }
            if self.cpu7.halted {
                let wake = if self.cpu7.intrwait_mask != 0 {
                    self.shared.irq7.has_matching_irq(self.cpu7.intrwait_mask)
                } else {
                    self.shared.irq7.has_unmasked_irq()
                };
                if wake {
                    self.cpu7.halted = false;
                    self.cpu7.intrwait_mask = 0;
                }
            }
        }
    }

    fn refresh_level_irqs(&mut self) {
        if self.shared.gpu3d.fifo.irq_condition() {
            self.shared.irq9.request(Irq::GxFifo);
        }
    }

    pub fn run_frame(&mut self) {
        self.run_cycles(CYCLES_PER_FRAME_ARM7);
    }

    pub fn step_one(&mut self) {
        if !self.cpu9.halted {
            self.ack_direct_boot_arm9_irq_without_handler();
            let itcm = self.cpu9.cp15.itcm;
            let dtcm = self.cpu9.cp15.dtcm;
            let mut bus = Bus9::new(&mut self.mem9, &mut self.shared, itcm, dtcm);
            self.cpu9.step(&mut bus);
        }
        if let Some(swi) = self.cpu9.pending_swi.take() {
            self.handle_swi9(swi);
        }
        if !self.cpu7.halted {
            let mut bus = Bus7::new(&mut self.mem7, &mut self.shared);
            let cycles = self.cpu7.step(&mut bus) as u64;
            self.scheduler.add_cycles(cycles);
            if let Some(swi) = self.cpu7.pending_swi.take() {
                self.handle_swi7(swi);
            }
            if self.shared.halt7_requested {
                self.cpu7.halted = true;
                self.shared.halt7_requested = false;
            }
        } else {
            self.scheduler.add_cycles(1);
        }
        while let Some(event) = self.scheduler.pop_if_ready() {
            self.dispatch_event(event);
        }
    }

    /// Update the keypad register from the frontend. `keys` is a 10-bit
    /// active-low value: bit n = 0 means button n is held. Layout:
    ///   0 A   1 B   2 Sel  3 Start  4 →  5 ←  6 ↑  7 ↓  8 R  9 L
    pub fn set_keys(&mut self, keys: u16) {
        self.shared.keyinput = keys & 0x03FF;
        self.check_keypad_irq();
    }

    /// EXTKEYIN — bits 0 = X, 1 = Y, 2 = (rsvd), 3 = debug, 6 = lid open,
    /// 7 = pen down. ARM7 only sees this register.
    pub fn set_extkeys(&mut self, extkeys: u16) {
        self.shared.extkeyin = extkeys & 0x007F;
    }

    /// Push touchscreen state from the frontend. `x` / `y` are NDS-screen
    /// pixel coords (0..256 / 0..192). `pen_down` controls the TSC pressure
    /// reading AND `EXTKEYIN` bit 6 (which is active-low: 0 = pen down).
    ///
    /// Real games either poll TSC over SPI directly, or read EXTKEYIN for
    /// a quick "is the stylus down?" check before going through SPI. We
    /// drive both paths from a single call.
    pub fn set_touch(&mut self, x: u16, y: u16, pen_down: bool) {
        self.shared.spi.tsc.set_touch(x, y, pen_down);
        // EXTKEYIN bit 6 = pen down (active-low). Clear it when pen is down.
        if pen_down {
            self.shared.extkeyin &= !(1 << 6);
        } else {
            self.shared.extkeyin |= 1 << 6;
        }
    }

    /// Select the cart backup type. Frontends should call this after
    /// loading a ROM if they have a save type to force.
    pub fn set_backup_kind(&mut self, kind: cart::BackupKind) {
        self.shared.auxspi.set_backup_kind(kind);
    }

    /// Import a `.sav` file into the AUXSPI backup.
    pub fn import_save(&mut self, data: &[u8]) {
        self.shared.auxspi.load_save(data);
    }

    /// Export a `.sav` from the AUXSPI backup. Returns `None` if no backup
    /// kind has been set.
    pub fn export_save(&self) -> Option<Vec<u8>> {
        self.shared.auxspi.export_save()
    }

    /// Drain stereo audio samples (interleaved L/R, signed 16-bit) into
    /// `out`. Returns the number of samples written. Padded with silence
    /// on underflow.
    pub fn drain_audio(&mut self, out: &mut [i16]) -> usize {
        self.shared.audio.drain(out)
    }

    fn check_keypad_irq(&mut self) {
        // For each CPU: KEYCNT[14] enables, KEYCNT[15] AND mode, KEYCNT[9:0]
        // selects which keys to test. Active-low convention: KEYINPUT bit
        // = 0 means held, so the test is on the *inverted* keyinput.
        let pressed = !self.shared.keyinput & 0x03FF;
        for (kcnt, irq) in [
            (self.shared.keycnt9, &mut self.shared.irq9),
            (self.shared.keycnt7, &mut self.shared.irq7),
        ] {
            if kcnt & (1 << 14) == 0 {
                continue;
            }
            let mask = kcnt & 0x03FF;
            let and_mode = kcnt & (1 << 15) != 0;
            let fire = if and_mode {
                pressed & mask == mask
            } else {
                pressed & mask != 0
            };
            if fire {
                irq.request(Irq::Keypad);
            }
        }
    }

    fn main_ram32(&self, addr: u32) -> u32 {
        let off = (addr as usize) & 0x3F_FFFF;
        u32::from_le_bytes([
            self.shared.main_ram[off],
            self.shared.main_ram[off + 1],
            self.shared.main_ram[off + 2],
            self.shared.main_ram[off + 3],
        ])
    }

    fn ack_direct_boot_arm9_irq_without_handler(&mut self) {
        if !self.direct_boot {
            return;
        }
        if !self.shared.irq9.has_pending() {
            return;
        }
        if self.mem9.has_installed_irq_vector() {
            return;
        }
        if self.main_ram32(0x02FF_3FFC) != 0 {
            return;
        }
        let pending = self.shared.irq9.ie & self.shared.irq9.iflag;
        self.set_direct_boot_arm9_irq_shadow(pending);
        self.shared.irq9.acknowledge(pending);
    }

    fn set_direct_boot_arm9_irq_shadow(&mut self, pending: u32) {
        if pending == 0 {
            return;
        }
        let dtcm = self.cpu9.cp15.dtcm;
        if dtcm.size_bytes < 8 {
            return;
        }
        let off = dtcm.size_bytes as usize - 8;
        let old = u32::from_le_bytes([
            self.mem9.dtcm[off],
            self.mem9.dtcm[off + 1],
            self.mem9.dtcm[off + 2],
            self.mem9.dtcm[off + 3],
        ]);
        self.mem9.dtcm[off..off + 4].copy_from_slice(&(old | pending).to_le_bytes());
    }

    fn run_dmas_for_timing9(&mut self, timing: dma::DmaTiming) {
        let channels = self.shared.dma9.channels_for_timing(timing);
        for ch in channels {
            let itcm = self.cpu9.cp15.itcm;
            let dtcm = self.cpu9.cp15.dtcm;
            let mut bus = Bus9::new(&mut self.mem9, &mut self.shared, itcm, dtcm);
            let irq = bus.run_dma(ch);
            if irq {
                let irq_bit = match ch {
                    0 => Irq::Dma0,
                    1 => Irq::Dma1,
                    2 => Irq::Dma2,
                    _ => Irq::Dma3,
                };
                self.shared.irq9.request(irq_bit);
            }
        }
    }

    fn run_dmas_for_timing7(&mut self, timing: dma::DmaTiming) {
        let channels = self.shared.dma7.channels_for_timing(timing);
        for ch in channels {
            let mut bus = Bus7::new(&mut self.mem7, &mut self.shared);
            let irq = bus.run_dma(ch);
            if irq {
                let irq_bit = match ch {
                    0 => Irq::Dma0,
                    1 => Irq::Dma1,
                    2 => Irq::Dma2,
                    _ => Irq::Dma3,
                };
                self.shared.irq7.request(irq_bit);
            }
        }
    }

    fn tick_timers(&mut self, arm9_cycles: u32, arm7_cycles: u32) {
        const TIMER_IRQS: [Irq; 4] = [Irq::Timer0, Irq::Timer1, Irq::Timer2, Irq::Timer3];

        let r9 = self.shared.timers9.tick(arm9_cycles);
        for (i, &fired) in r9.irqs.iter().enumerate() {
            if fired {
                self.shared.irq9.request(TIMER_IRQS[i]);
            }
        }

        let r7 = self.shared.timers7.tick(arm7_cycles);
        for (i, &fired) in r7.irqs.iter().enumerate() {
            if fired {
                self.shared.irq7.request(TIMER_IRQS[i]);
            }
        }
    }

    fn handle_swi9(&mut self, swi: u8) {
        let real_bios = !self.direct_boot;
        if real_bios {
            // With a real BIOS dump the CPU enters the standard SWI vector.
            self.cpu9.software_interrupt(swi as u32);
        } else {
            let itcm = self.cpu9.cp15.itcm;
            let dtcm = self.cpu9.cp15.dtcm;
            let mut bus = Bus9::new(&mut self.mem9, &mut self.shared, itcm, dtcm);
            if !bios::arm9::handle_swi(&mut self.cpu9, &mut bus, swi) {
                log::trace!("ARM9 direct-boot unhandled SWI 0x{:02X}; returning", swi);
            }
        }
    }

    fn handle_swi7(&mut self, swi: u8) {
        let real_bios = !self.direct_boot;
        if real_bios {
            self.cpu7.software_interrupt(swi as u32);
        } else {
            let mut bus = Bus7::new(&mut self.mem7, &mut self.shared);
            if !bios::arm7::handle_swi(&mut self.cpu7, &mut bus, swi) {
                log::trace!("ARM7 direct-boot unhandled SWI 0x{:02X}; returning", swi);
            }
        }
    }

    fn dispatch_event(&mut self, event: Event) {
        let now = self.scheduler.timestamp();
        match event.kind {
            EventKind::HBlank => {
                self.shared.dispstat9 |= 0x0002;
                self.shared.dispstat7 |= 0x0002;

                // HBlank-triggered DMA on ARM9 — visible scanlines only.
                if self.shared.vcount < VISIBLE_LINES {
                    self.run_dmas_for_timing9(dma::DmaTiming::HBlankVisible);
                }

                // Render the current scanline before advancing — `vcount`
                // hasn't moved yet so we paint line N during line N's HBlank.
                let line = self.shared.vcount;
                if line < VISIBLE_LINES {
                    let swap_lcd = self.shared.powcnt1 & (1 << 15) != 0;
                    let (top_engine_a, bot_engine_a) = if swap_lcd {
                        (false, true)
                    } else {
                        (true, false)
                    };
                    // Engine A → top by default. Pass the 3D framebuffer
                    // slice so BG0 can read from it when DISPCNT bit 3 is set.
                    {
                        let palette = &self.shared.palette[0..0x400];
                        let oam = &self.shared.oam[0..0x400];
                        let fb_3d: &[u16] = &self.shared.gpu3d.rasterizer.framebuffer;
                        let fb = if top_engine_a {
                            &mut self.framebuffer_top
                        } else {
                            &mut self.framebuffer_bot
                        };
                        gpu2d::render_scanline(
                            &mut self.shared.engine_a,
                            line,
                            palette,
                            oam,
                            &self.shared.vram,
                            fb,
                            Some(fb_3d),
                        );
                    }
                    // Engine B → bottom by default. Engine B has no 3D source.
                    {
                        let palette = &self.shared.palette[0x400..0x800];
                        let oam = &self.shared.oam[0x400..0x800];
                        let fb = if bot_engine_a {
                            &mut self.framebuffer_top
                        } else {
                            &mut self.framebuffer_bot
                        };
                        gpu2d::render_scanline(
                            &mut self.shared.engine_b,
                            line,
                            palette,
                            oam,
                            &self.shared.vram,
                            fb,
                            None,
                        );
                    }
                }

                if self.shared.vcount < VISIBLE_LINES {
                    if self.shared.dispstat9 & 0x0010 != 0 {
                        self.shared.irq9.request(Irq::HBlank);
                    }
                    if self.shared.dispstat7 & 0x0010 != 0 {
                        self.shared.irq7.request(Irq::HBlank);
                    }
                }

                self.scheduler.schedule(Event {
                    fire_time: now + HBLANK_CYCLES_ARM7 as u64,
                    kind: EventKind::HBlankEnd,
                });
            }
            EventKind::HBlankEnd => {
                // Clear HBlank flag, advance VCOUNT
                self.shared.dispstat9 &= !0x0002;
                self.shared.dispstat7 &= !0x0002;
                self.shared.vcount = (self.shared.vcount + 1) % LINES_PER_FRAME;
                let line = self.shared.vcount;

                // VCount-match check — DISPSTAT[15:8] holds LYC for both CPUs separately
                check_vcount_match(&mut self.shared.dispstat9, &mut self.shared.irq9, line);
                check_vcount_match(&mut self.shared.dispstat7, &mut self.shared.irq7, line);

                if line == VISIBLE_LINES {
                    // Enter VBlank
                    self.shared.dispstat9 |= 0x0001;
                    self.shared.dispstat7 |= 0x0001;
                    if self.shared.dispstat9 & 0x0008 != 0 {
                        self.shared.irq9.request(Irq::VBlank);
                    }
                    if self.shared.dispstat7 & 0x0008 != 0 {
                        self.shared.irq7.request(Irq::VBlank);
                    }
                    // VBlank-triggered DMAs on both CPUs.
                    self.run_dmas_for_timing9(dma::DmaTiming::VBlank);
                    self.run_dmas_for_timing7(dma::DmaTiming::VBlank);
                    self.scheduler.schedule(Event {
                        fire_time: now,
                        kind: EventKind::VBlank,
                    });
                } else if line == 0 {
                    // VBlank ends — re-latch affine reference points for both
                    // engines and swap the 3D engine's polygon buffer for
                    // the frame about to render.
                    self.shared.dispstat9 &= !0x0001;
                    self.shared.dispstat7 &= !0x0001;
                    self.shared.engine_a.latch_affine_refs();
                    self.shared.engine_b.latch_affine_refs();
                    // Disjoint-borrow the gpu3d and vram fields so the
                    // rasterizer can read textures.
                    let SharedState { gpu3d, vram, .. } = &mut self.shared;
                    gpu3d.swap_buffers(Some(vram));
                }

                self.scheduler.schedule(Event {
                    fire_time: now + HDRAW_CYCLES_ARM7 as u64,
                    kind: EventKind::HBlank,
                });
            }
            EventKind::VBlank => {
                // VBlank entry already handled in HBlankEnd; this event slot
                // is reserved for VBlank DMA / capture wiring in Phase 3+.
            }
            _ => {
                // Phase 2 doesn't wire these yet.
            }
        }
    }
}

fn check_vcount_match(dispstat: &mut u16, irq: &mut InterruptController, line: u16) {
    // Combined LYC: DISPSTAT[15:8] is the low 8 bits and bit 7 is the 9th bit.
    let lyc = ((*dispstat >> 8) & 0xFF) | (((*dispstat >> 7) & 1) << 8);
    if line == lyc {
        *dispstat |= 0x0004;
        if *dispstat & 0x0020 != 0 {
            irq.request(Irq::VCountMatch);
        }
    } else {
        *dispstat &= !0x0004;
    }
}

#[derive(Debug)]
pub enum CartLoadError {
    Header(cart::ParseError),
    DirectBoot(cart::DirectBootError),
}

impl std::fmt::Display for CartLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CartLoadError::Header(e) => write!(f, "header parse failed: {}", e),
            CartLoadError::DirectBoot(e) => write!(f, "direct boot failed: {}", e),
        }
    }
}

impl std::error::Error for CartLoadError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lockstep_steps_both_cpus() {
        let mut nds = Nds::new(None, None);

        for i in 0..16 {
            let off = i * 4;
            let bytes = 0xE1A0_0000u32.to_le_bytes();
            for b in 0..4 {
                nds.shared.main_ram[off + b] = bytes[b];
            }
        }

        nds.cpu9.cpsr = Psr::new(CpuMode::System);
        nds.cpu9.cpsr.bits &= !(1 << 7);
        nds.cpu9.regs[15] = 0x0200_0000;
        nds.cpu9.pipeline_flushed = true;

        nds.cpu7.cpsr = Psr::new(CpuMode::System);
        nds.cpu7.cpsr.bits &= !(1 << 7);
        nds.cpu7.regs[15] = 0x0200_0000;
        nds.cpu7.pipeline_flushed = true;

        for _ in 0..4 {
            nds.step_one();
        }

        assert!(
            nds.cpu9.regs[15] >= 0x0200_0010,
            "ARM9 PC didn't advance: 0x{:08X}",
            nds.cpu9.regs[15]
        );
        assert!(
            nds.cpu7.regs[15] >= 0x0200_0010,
            "ARM7 PC didn't advance: 0x{:08X}",
            nds.cpu7.regs[15]
        );
    }

    #[test]
    fn test_save_state_round_trip() {
        let mut nds = Nds::new(None, None);
        nds.shared.main_ram[0x100] = 0xAB;
        nds.cpu9.regs[3] = 0xCAFE_BABE;
        nds.cpu7.regs[5] = 0xDEAD_BEEF;
        let bytes = bincode::serialize(&nds).expect("serialize");
        let restored: Nds = bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(restored.shared.main_ram[0x100], 0xAB);
        assert_eq!(restored.cpu9.regs[3], 0xCAFE_BABE);
        assert_eq!(restored.cpu7.regs[5], 0xDEAD_BEEF);
    }

    #[test]
    fn test_direct_boot_arm9_irq_without_handler_is_acked() {
        let mut nds = Nds::new(None, None);
        nds.direct_boot = true;
        nds.cpu7.halted = true;
        nds.cpu9.cp15.dtcm = cpu::cp15::TcmRegion {
            base: 0x0B00_0000,
            size_bytes: 16 * 1024,
        };

        let nop = 0xE1A0_0000u32.to_le_bytes();
        nds.shared.main_ram[0..4].copy_from_slice(&nop);
        nds.cpu9.cpsr = Psr::new(CpuMode::System);
        nds.cpu9.cpsr.bits &= !(1 << 7);
        nds.cpu9.regs[15] = 0x0200_0000;
        nds.cpu9.pipeline_flushed = true;
        nds.shared.irq9.write_ie(Irq::VBlank.bit());
        nds.shared.irq9.write_ime(1);
        nds.shared.irq9.request(Irq::VBlank);

        nds.step_one();

        assert_eq!(nds.shared.irq9.iflag & Irq::VBlank.bit(), 0);
        assert_eq!(nds.cpu9.irq_entries, 0);
        assert_ne!(nds.cpu9.regs[15] & 0xFFFF_0000, 0xFFFF_0000);
        let shadow_off = nds.cpu9.cp15.dtcm.size_bytes as usize - 8;
        let shadow = u32::from_le_bytes([
            nds.mem9.dtcm[shadow_off],
            nds.mem9.dtcm[shadow_off + 1],
            nds.mem9.dtcm[shadow_off + 2],
            nds.mem9.dtcm[shadow_off + 3],
        ]);
        assert_eq!(shadow & Irq::VBlank.bit(), Irq::VBlank.bit());
    }

    #[test]
    fn test_direct_boot_arm9_irq_with_itcm_vector_is_not_acked_by_fallback() {
        let mut nds = Nds::new(None, None);
        nds.direct_boot = true;
        nds.cpu7.halted = true;

        let nop = 0xE1A0_0000u32.to_le_bytes();
        nds.shared.main_ram[0..4].copy_from_slice(&nop);
        nds.mem9.itcm[0x18..0x1C].copy_from_slice(&0xE59F_F010u32.to_le_bytes());
        nds.cpu9.cpsr = Psr::new(CpuMode::System);
        nds.cpu9.cpsr.bits &= !(1 << 7);
        nds.cpu9.regs[15] = 0x0200_0000;
        nds.cpu9.pipeline_flushed = true;
        nds.shared.irq9.write_ie(Irq::IpcSync.bit());
        nds.shared.irq9.write_ime(1);
        nds.shared.irq9.request(Irq::IpcSync);

        nds.step_one();

        assert_ne!(nds.shared.irq9.iflag & Irq::IpcSync.bit(), 0);
        assert_eq!(nds.cpu9.irq_entries, 1);
    }

    /// Run a frame with VBlank IRQ enabled in DISPSTAT and IE on both CPUs.
    /// After ~1 frame, both CPUs' IF should have the VBlank bit set.
    #[test]
    fn test_vblank_irq_fires_on_both_cpus() {
        let mut nds = Nds::new(None, None);

        // Both CPUs spin at a B . instruction in main RAM (so they keep
        // executing — halted CPUs return without setting timestamps the way
        // we'd expect for this test).
        for i in 0..4 {
            let bytes = 0xEAFF_FFFEu32.to_le_bytes(); // B .
            for b in 0..4 {
                nds.shared.main_ram[i * 4 + b] = bytes[b];
            }
        }
        nds.cpu9.cpsr = Psr::new(CpuMode::System);
        nds.cpu9.cpsr.bits &= !(1 << 7);
        nds.cpu9.regs[15] = 0x0200_0000;
        nds.cpu9.pipeline_flushed = true;
        nds.cpu7.cpsr = Psr::new(CpuMode::System);
        nds.cpu7.cpsr.bits &= !(1 << 7);
        nds.cpu7.regs[15] = 0x0200_0000;
        nds.cpu7.pipeline_flushed = true;

        // Enable VBlank IRQ in DISPSTAT (bit 3) and IE (bit 0). Don't set
        // IME so the CPU itself doesn't actually take the IRQ — we just want
        // to assert the controller flagged it.
        nds.shared.dispstat9 = 0x0008;
        nds.shared.dispstat7 = 0x0008;
        nds.shared.irq9.write_ie(Irq::VBlank.bit());
        nds.shared.irq7.write_ie(Irq::VBlank.bit());

        nds.run_frame();

        assert!(
            nds.shared.irq9.read_if() & Irq::VBlank.bit() != 0,
            "ARM9 IF should have VBlank set"
        );
        assert!(
            nds.shared.irq7.read_if() & Irq::VBlank.bit() != 0,
            "ARM7 IF should have VBlank set"
        );
    }

    #[test]
    fn test_vcount_advances_through_full_frame() {
        let mut nds = Nds::new(None, None);
        // Both CPUs start halted so we don't burn time decoding garbage —
        // the scheduler still advances.
        nds.cpu9.halted = true;
        nds.cpu7.halted = true;

        nds.run_frame();
        // After exactly one frame, vcount should be back to 0 (we cycle
        // through 0..LINES_PER_FRAME and the count is mod LINES_PER_FRAME).
        assert_eq!(nds.shared.vcount, 0);
    }

    /// Regression for the halt-wake bug inherited from `../gba` (their
    /// commit 27722c4). `run_cycles` skips `step()` while a CPU is halted,
    /// but `step()` is the only place that clears `halted` — so without
    /// the post-dispatch halt-wake check, a CPU that executes `SWI Halt` /
    /// `IntrWait` / `VBlankIntrWait` sleeps forever even when VBlank fires.
    ///
    /// Real ARM7TDMI / ARM946E-S wakes on `(IE & IF) != 0` alone,
    /// regardless of IME and CPSR.I. See
    /// `debug/2026-05-08_halt-wake-inherited.md`.
    #[test]
    fn test_halt_wake_on_unmasked_vblank_irq() {
        let mut nds = Nds::new(None, None);
        // Configure VBlank IRQ enable in each CPU's DISPSTAT (bit 3) and
        // unmask it in IE. Crucially, IME is left at 0 — that gates
        // delivery but must NOT gate halt-wake.
        nds.shared.dispstat9 = 0x0008;
        nds.shared.dispstat7 = 0x0008;
        nds.shared.irq9.write_ie(Irq::VBlank.bit());
        nds.shared.irq7.write_ie(Irq::VBlank.bit());
        // IME is 0 by default — make it explicit.
        nds.shared.irq9.write_ime(0);
        nds.shared.irq7.write_ime(0);

        // Halt both CPUs (as if they just called SWI 0x05 VBlankIntrWait).
        nds.cpu9.halted = true;
        nds.cpu7.halted = true;

        // Pre-fix: run_frame() never wakes either CPU even though
        // VBlank fires at line 192 and sets IF.VBlank on both controllers.
        // Post-fix: halt-wake clears `halted` after dispatch_event drains
        // the queue with a VBlank pending.
        nds.run_frame();

        assert!(
            nds.shared.irq9.read_if() & Irq::VBlank.bit() != 0,
            "ARM9 IF should have VBlank pending"
        );
        assert!(
            nds.shared.irq7.read_if() & Irq::VBlank.bit() != 0,
            "ARM7 IF should have VBlank pending"
        );
        assert!(
            !nds.cpu9.halted,
            "ARM9 should be woken: IE & IF != 0 even with IME=0"
        );
        assert!(
            !nds.cpu7.halted,
            "ARM7 should be woken: IE & IF != 0 even with IME=0"
        );
    }

    /// Negative case: halt-wake must NOT fire when no IRQ source is enabled.
    /// The CPU should stay halted, otherwise we've broken the basic halt
    /// behavior.
    #[test]
    fn test_halt_stays_halted_when_no_irq_enabled() {
        let mut nds = Nds::new(None, None);
        // No IE bits set, no DISPSTAT enables.
        nds.cpu9.halted = true;
        nds.cpu7.halted = true;
        nds.run_frame();
        assert!(
            nds.cpu9.halted,
            "ARM9 should remain halted (no IRQ enabled)"
        );
        assert!(
            nds.cpu7.halted,
            "ARM7 should remain halted (no IRQ enabled)"
        );
    }

    #[test]
    fn test_arm7_haltcnt_write_halts_until_enabled_irq() {
        let mut nds = Nds::new(None, None);
        {
            let mut bus = Bus7::new(&mut nds.mem7, &mut nds.shared);
            bus.write8(0x0400_0301, 0x80);
        }

        nds.step_one();

        assert!(nds.cpu7.halted, "HALTCNT bit 7 should park ARM7");
    }

    /// Regression for the IntrWait bug inherited from `../gba`
    /// (their commit `bb4b916`, FE7 cascade). Real BIOS implements SWI 0x04
    /// / 0x05 as `loop { HALT; if BIOS_IF & mask: break; }` — only an IRQ
    /// whose bit matches the wait-mask wakes the CPU. Without this gate,
    /// any unrelated IRQ (HBlank, Timer, IPC) wakes the CPU prematurely and
    /// the game's main loop iterates many times per frame instead of once.
    ///
    /// Here we park the CPU under VBlankIntrWait semantics (mask = VBlank
    /// bit), then raise an HBlank IRQ. The CPU must stay halted.
    #[test]
    fn test_intrwait_mask_blocks_non_matching_irq() {
        let mut nds = Nds::new(None, None);
        // Enable both VBlank and HBlank IRQs so a real IRQ source could be
        // pending. The mask, not IE, is what gates the wake.
        nds.shared.dispstat9 = 0x0018; // VBlank + HBlank enabled
        nds.shared
            .irq9
            .write_ie(Irq::VBlank.bit() | Irq::HBlank.bit());

        // Park CPU as if SWI 0x05 just ran — mask = VBlank only.
        nds.cpu9.halted = true;
        nds.cpu9.intrwait_mask = Irq::VBlank.bit();

        // Raise HBlank, NOT VBlank. (Direct request to avoid running enough
        // cycles to hit line 192 and trigger VBlank too.)
        nds.shared.irq9.request(Irq::HBlank);

        // Step one of the halt-wake checks: we don't want VBlank to actually
        // fire from the scheduler, so just poke the halt-wake path by
        // running a tiny chunk of cycles. HBlank fires every 1118 cycles on
        // ARM7-domain; 100 cycles is short enough that no scheduled event
        // races us.
        nds.run_cycles(100);

        assert!(
            nds.cpu9.halted,
            "ARM9 should stay halted: HBlank fired but mask only matches VBlank"
        );
        assert_eq!(
            nds.cpu9.intrwait_mask,
            Irq::VBlank.bit(),
            "Mask should remain set while waiting"
        );
    }

    /// Companion to the above: when the *matching* IRQ fires, the CPU
    /// must wake AND the mask must clear (so a subsequent HALTCNT-style
    /// halt isn't gated by a stale mask).
    #[test]
    fn test_intrwait_mask_wakes_on_matching_irq() {
        let mut nds = Nds::new(None, None);
        nds.shared.dispstat9 = 0x0008; // VBlank IRQ enabled
        nds.shared.irq9.write_ie(Irq::VBlank.bit());

        nds.cpu9.halted = true;
        nds.cpu9.intrwait_mask = Irq::VBlank.bit();

        // Raise VBlank directly.
        nds.shared.irq9.request(Irq::VBlank);
        nds.run_cycles(100);

        assert!(
            !nds.cpu9.halted,
            "ARM9 should wake: VBlank matches the IntrWait mask"
        );
        assert_eq!(
            nds.cpu9.intrwait_mask, 0,
            "Mask should clear on wake so future HALTCNT halts aren't gated"
        );
    }

    #[test]
    fn test_io_register_round_trip_arm9_ie() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );
        bus.write32(0x0400_0210, 0xDEAD_BEEF);
        assert_eq!(bus.read32(0x0400_0210), 0xDEAD_BEEF);
    }

    #[test]
    fn test_alpha_test_ref_masks_to_five_bits() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write16(0x0400_0340, 0x00F2);

        assert_eq!(bus.shared.gpu3d.rasterizer.alpha_test_ref, 0x12);
    }

    #[test]
    fn test_disp3dcnt_write_ignores_status_and_unused_bits() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write16(0x0400_0060, 0xFFFF);

        assert_eq!(bus.shared.gpu3d.rasterizer.disp3dcnt, 0x4FFF);
    }

    #[test]
    fn test_io_register_per_cpu_isolation() {
        let mut nds = Nds::new(None, None);
        // Write IE on ARM9 — should not be visible from ARM7's IE.
        {
            let mut bus = Bus9::new(
                &mut nds.mem9,
                &mut nds.shared,
                nds.cpu9.cp15.itcm,
                nds.cpu9.cp15.dtcm,
            );
            bus.write32(0x0400_0210, 0x0000_0001);
        }
        {
            let mut bus = Bus7::new(&mut nds.mem7, &mut nds.shared);
            assert_eq!(
                bus.read32(0x0400_0210),
                0,
                "ARM7's IE register should be independent of ARM9's"
            );
        }
    }

    #[test]
    fn test_direct_boot_via_top_level() {
        let rom = make_synth_rom();
        let mut nds = Nds::new(None, None);
        nds.load_cart_direct_boot(rom).expect("direct boot");

        assert_eq!(nds.cpu9.regs[15], 0x0200_0000);
        assert_eq!(nds.cpu7.regs[15], 0x0238_0000);
        assert!(nds.direct_boot);
        assert!(nds.cart.header().is_some());
    }

    /// Helper: configure Engine A to text mode 0 with BG0 enabled, route
    /// VRAM bank A to Engine A BG, and stamp a single 8×8 4bpp tile of
    /// palette index 1 at tile 0. Returns palette color used.
    fn setup_solid_red_bg0(nds: &mut Nds) -> u16 {
        // Bank A → Engine A BG (mst=1, offset=0)
        nds.shared.vram.write_cnt(crate::vram::BankId::A, 0x80 | 1);

        // Map block 0 stays at offset 0 (all zeros = tile 0, palette 0).
        // Char block 1 (offset 0x4000) gets tile 0's 4bpp pixel data: 32
        // bytes of 0x11 (each byte holds two palette-index-1 pixels).
        for i in 0..32 {
            nds.shared.vram.cpu_write_arm9(0x0600_4000 + i, 0x11);
        }

        // BG0CNT: priority 0, char_base 1, screen_base 0, 4bpp, 32x32 size.
        nds.shared.engine_a.bgcnt[0] = 1 << 2;
        // Mode 0, BG0 enable (DISPCNT bit 8), display mode 1 (normal).
        nds.shared.engine_a.dispcnt = 0x0001_0100;

        // Engine A palette[1] = red (BGR555: red = 0x001F).
        let red = 0x001Fu16;
        nds.shared.palette[2] = red as u8;
        nds.shared.palette[3] = (red >> 8) as u8;
        red
    }

    #[test]
    fn test_solid_bg_renders_to_top_framebuffer() {
        let mut nds = Nds::new(None, None);
        let red = setup_solid_red_bg0(&mut nds);
        // Both CPUs halted so we just drive the scheduler.
        nds.cpu9.halted = true;
        nds.cpu7.halted = true;

        nds.run_frame();

        // Every visible pixel on the top framebuffer should be red.
        for y in 0..SCREEN_HEIGHT {
            for x in 0..SCREEN_WIDTH {
                assert_eq!(
                    nds.framebuffer_top[y * SCREEN_WIDTH + x],
                    red,
                    "pixel ({},{}) wrong: 0x{:04X}",
                    x,
                    y,
                    nds.framebuffer_top[y * SCREEN_WIDTH + x]
                );
            }
        }
    }

    #[test]
    fn test_direct_vram_display_mode_renders_lcdc_block() {
        let mut nds = Nds::new(None, None);
        nds.cpu9.halted = true;
        nds.cpu7.halted = true;
        nds.shared.engine_a.dispcnt = 2 << 16;
        nds.shared.vram.write_cnt(crate::vram::BankId::A, 0x80);
        nds.shared.vram.cpu_write_arm9(0x0680_0000, 0x1F);
        nds.shared.vram.cpu_write_arm9(0x0680_0001, 0x00);

        nds.run_frame();

        assert_eq!(nds.framebuffer_top[0], 0x001F);
    }

    #[test]
    fn test_powcnt_swap_lcd_routes_engine_a_to_bottom() {
        let mut nds = Nds::new(None, None);
        let red = setup_solid_red_bg0(&mut nds);
        // POWCNT1 bit 15 = swap LCDs.
        nds.shared.powcnt1 = 1 << 15;
        nds.cpu9.halted = true;
        nds.cpu7.halted = true;

        nds.run_frame();

        // Engine A's red should now be on the BOTTOM framebuffer.
        assert_eq!(nds.framebuffer_bot[0], red);
        // Top should be backdrop (palette[0] = 0 → black) since Engine B has
        // no BGs configured.
        assert_eq!(nds.framebuffer_top[0], 0);
    }

    #[test]
    fn test_obj_renders_above_bg() {
        let mut nds = Nds::new(None, None);
        let _red = setup_solid_red_bg0(&mut nds);

        // OBJ palette index 1 → blue (BGR555 0x7C00).
        let blue = 0x7C00u16;
        nds.shared.palette[0x200 + 2] = blue as u8;
        nds.shared.palette[0x200 + 3] = (blue >> 8) as u8;

        // Bank B → Engine A OBJ (mst=2, offset=0)
        nds.shared.vram.write_cnt(crate::vram::BankId::B, 0x80 | 2);

        // OBJ tile 0: 32 bytes of 0x11. With 1D mapping and the default
        // boundary of 32 bytes (DISPCNT bits 20-21 = 0), tile 0 lives at
        // OBJ VRAM offset 0.
        for i in 0..32 {
            nds.shared.vram.cpu_write_arm9(0x0640_0000 + i, 0x11);
        }

        // OAM entry 0: an 8x8 sprite at (0, 0), tile 0, priority 0.
        // attr0: y=0, mode=0 (normal), gfx=0, mosaic=0, 4bpp, shape=0
        // attr1: x=0, hflip=0, vflip=0, size=0
        // attr2: tile=0, priority=0, palette=0
        let oam = &mut nds.shared.oam;
        oam[0] = 0;
        oam[1] = 0; // attr0
        oam[2] = 0;
        oam[3] = 0; // attr1
        oam[4] = 0;
        oam[5] = 0; // attr2

        // DISPCNT: enable BG0 (bit 8) AND OBJ (bit 12), 1D mapping (bit 4),
        // mode=0, display_mode=1.
        nds.shared.engine_a.dispcnt = 0x0001_1110;

        nds.cpu9.halted = true;
        nds.cpu7.halted = true;
        nds.run_frame();

        // Pixel (0,0) should be blue (OBJ on top of red BG).
        assert_eq!(nds.framebuffer_top[0], blue);
        // Pixel (10,0) is outside the 8x8 sprite — should be red (BG).
        let red = 0x001Fu16;
        assert_eq!(nds.framebuffer_top[10], red);
    }

    fn make_synth_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x4400];
        rom[0x4000..0x4004].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
        rom[0x4200..0x4204].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
        rom[0x20..0x24].copy_from_slice(&0x4000u32.to_le_bytes());
        rom[0x24..0x28].copy_from_slice(&0x0200_0000u32.to_le_bytes());
        rom[0x28..0x2C].copy_from_slice(&0x0200_0000u32.to_le_bytes());
        rom[0x2C..0x30].copy_from_slice(&4u32.to_le_bytes());
        rom[0x30..0x34].copy_from_slice(&0x4200u32.to_le_bytes());
        rom[0x34..0x38].copy_from_slice(&0x0238_0000u32.to_le_bytes());
        rom[0x38..0x3C].copy_from_slice(&0x0238_0000u32.to_le_bytes());
        rom[0x3C..0x40].copy_from_slice(&4u32.to_le_bytes());
        let crc = cart::header::crc16_modbus(&rom[..0x15E]);
        rom[0x15E..0x160].copy_from_slice(&crc.to_le_bytes());
        rom
    }

    #[test]
    fn test_ipc_fifo_arm9_to_arm7_round_trip() {
        let mut nds = Nds::new(None, None);

        // Both CPUs enable their FIFOs via the I/O bus.
        {
            let mut bus = Bus9::new(
                &mut nds.mem9,
                &mut nds.shared,
                nds.cpu9.cp15.itcm,
                nds.cpu9.cp15.dtcm,
            );
            bus.write16(0x0400_0184, 1 << 15); // FIFOCNT enable
        }
        {
            let mut bus = Bus7::new(&mut nds.mem7, &mut nds.shared);
            bus.write16(0x0400_0184, 1 << 15);
        }

        // ARM9 sends 4 words through 0x04000188.
        let payload = [0xCAFE_BABE, 0xDEAD_BEEF, 0x1111_2222, 0xABCD_0123];
        {
            let mut bus = Bus9::new(
                &mut nds.mem9,
                &mut nds.shared,
                nds.cpu9.cp15.itcm,
                nds.cpu9.cp15.dtcm,
            );
            for w in payload {
                bus.write32(0x0400_0188, w);
            }
        }

        // ARM7 reads them in order from 0x04100000.
        {
            let mut bus = Bus7::new(&mut nds.mem7, &mut nds.shared);
            for w in payload {
                assert_eq!(bus.read32(0x0410_0000), w);
            }
        }
    }

    #[test]
    fn test_ipc_sync_trigger_raises_irq_on_other_cpu() {
        let mut nds = Nds::new(None, None);

        // ARM7 enables receive-IRQ in IPCSYNC bit 14, and IPC-Sync in IE.
        {
            let mut bus = Bus7::new(&mut nds.mem7, &mut nds.shared);
            bus.write16(0x0400_0180, 1 << 14);
            bus.write32(0x0400_0210, Irq::IpcSync.bit());
        }

        // ARM9 writes IPCSYNC with bit 13 (trigger).
        {
            let mut bus = Bus9::new(
                &mut nds.mem9,
                &mut nds.shared,
                nds.cpu9.cp15.itcm,
                nds.cpu9.cp15.dtcm,
            );
            bus.write16(0x0400_0180, (0xA << 8) | (1 << 13));
        }

        assert!(
            nds.shared.irq7.read_if() & Irq::IpcSync.bit() != 0,
            "ARM7's IF should have IpcSync set"
        );
        // ARM9's IF should be untouched.
        assert_eq!(nds.shared.irq9.read_if() & Irq::IpcSync.bit(), 0);
    }

    #[test]
    fn test_dma9_immediate_word_copy() {
        let mut nds = Nds::new(None, None);
        // Source words at 0x02000000.
        for i in 0..16u32 {
            let bytes = (0x1000u32 + i).to_le_bytes();
            for b in 0..4 {
                nds.shared.main_ram[i as usize * 4 + b] = bytes[b];
            }
        }
        // Configure DMA channel 0 via the bus: SAD=0x02000000, DAD=0x02001000,
        // CNT = enable + word transfer + count 16 + immediate.
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0200_1000);
        // CNT: enable (1<<31), word size (1<<26), src+dst increment (default),
        // count = 16. Timing = immediate (bits 27:29 = 0).
        bus.write32(0x0400_00B8, (1u32 << 31) | (1 << 26) | 16);

        // After the immediate-mode write, the transfer should be done.
        // Verify destination matches source.
        for i in 0..16u32 {
            let off = 0x1000 + i as usize * 4;
            let v = u32::from_le_bytes([
                nds.shared.main_ram[off],
                nds.shared.main_ram[off + 1],
                nds.shared.main_ram[off + 2],
                nds.shared.main_ram[off + 3],
            ]);
            assert_eq!(v, 0x1000 + i, "word {} mismatch", i);
        }
    }

    #[test]
    fn test_dma9_vblank_fires_at_line_192() {
        let mut nds = Nds::new(None, None);
        // Source word at 0x02000000.
        nds.shared.main_ram[0..4].copy_from_slice(&0xCAFE_BABEu32.to_le_bytes());

        // Configure DMA channel 0 for VBlank trigger.
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );
        bus.write32(0x0400_00B0, 0x0200_0000);
        bus.write32(0x0400_00B4, 0x0200_2000);
        // VBlank trigger = bits 27:29 = 001 → bit 27 set
        bus.write32(0x0400_00B8, (1u32 << 31) | (1 << 26) | (1 << 27) | 1);
        drop(bus);

        // Halt both CPUs and run a frame.
        nds.cpu9.halted = true;
        nds.cpu7.halted = true;
        nds.run_frame();

        // After VBlank fired, the destination should hold the source word.
        let v = u32::from_le_bytes([
            nds.shared.main_ram[0x2000],
            nds.shared.main_ram[0x2001],
            nds.shared.main_ram[0x2002],
            nds.shared.main_ram[0x2003],
        ]);
        assert_eq!(v, 0xCAFE_BABE);
    }

    #[test]
    fn test_timer0_overflow_irq() {
        let mut nds = Nds::new(None, None);
        // Reload = 0xFFFE; with prescaler 1 and ARM9 stepping ~2 cycles per
        // ARM7 step, the timer should overflow within a few outer loops.
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );
        bus.write16(0x0400_0100, 0xFFFE); // TM0CNT_L (reload)
        bus.write16(0x0400_0102, (1 << 7) | (1 << 6)); // enable + IRQ, prescaler=0 (F/1)
        drop(bus);

        // Both CPUs halted — only timers tick from the run loop.
        nds.cpu9.halted = true;
        nds.cpu7.halted = true;
        // Run for a frame to give the timer plenty of cycles.
        nds.run_frame();
        assert!(
            nds.shared.irq9.read_if() & Irq::Timer0.bit() != 0,
            "Timer0 IRQ should have fired, IF = 0x{:08X}",
            nds.shared.irq9.read_if()
        );
    }

    /// End-to-end Phase 7 test: configure 3D + DISPCNT.bg0_3d + a single
    /// red triangle, run a frame, verify the top framebuffer has red
    /// pixels (the rasterized 3D landing through Engine A BG0).
    #[test]
    fn test_audio_drain_produces_samples_each_frame() {
        let mut nds = Nds::new(None, None);
        // Master enable + full master volume.
        nds.shared.audio.master_cnt = (1 << 15) | 127;
        // Plant a PCM8 sample buffer in main RAM at 0x100.
        for i in 0..32 {
            nds.shared.main_ram[0x100 + i] = (i as u8).wrapping_mul(8);
        }
        // Channel 0: PCM8, full ch volume, center pan, loop mode, start.
        nds.shared.audio.channels[0].sad = 0x0200_0100;
        nds.shared.audio.channels[0].tmr = 0xFF00; // ~256-cycle period
        nds.shared.audio.channels[0].pnt = 0;
        nds.shared.audio.channels[0].len = 8;
        let cnt = (1 << 31) | (1 << 27) | (64 << 16) | 127; // start+loop+pan64+vol127
        nds.shared.audio.write_cnt(0, cnt);

        nds.cpu9.halted = true;
        nds.cpu7.halted = true;
        nds.run_frame();

        // After a frame, drain — we should get a bunch of samples.
        let mut buf = [0i16; 2048];
        let n = nds.drain_audio(&mut buf);
        assert!(
            n >= 1024,
            "expected at least 1024 samples per frame, got {}",
            n
        );
        // Some samples should be non-zero (the PCM8 buffer has variation).
        let nonzero = buf[..n].iter().filter(|&&s| s != 0).count();
        assert!(
            nonzero > 100,
            "expected non-silent samples; got only {} nonzero",
            nonzero
        );
    }

    #[test]
    fn test_audio_register_round_trip_via_arm7_bus() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus7::new(&mut nds.mem7, &mut nds.shared);
        // Write SOUND0CNT_L = 0x12345678
        bus.write32(0x0400_0400, 0x1234_5678);
        // Read back through the bus.
        let v = bus.read32(0x0400_0400);
        // High bit (31) reflects active state; if no start transition,
        // CNT reads back as written.
        assert_eq!(v & 0x7FFF_FFFF, 0x1234_5678 & 0x7FFF_FFFF);
    }

    #[test]
    fn test_3d_rasterized_triangle_lands_on_top_framebuffer() {
        let mut nds = Nds::new(None, None);

        // Configure Engine A: display mode 1, BG0 enabled, BG0_3D enabled.
        // DISPCNT bits: [0..2] mode, [3] bg0_3d, [8] bg0_enable, [16..17] display mode.
        nds.shared.engine_a.dispcnt = 0x0001_0108; // mode=0, bg0_3d=1, bg0_en=1, dispmode=1

        // Configure rasterizer: 3D enabled.
        nds.shared.gpu3d.rasterizer.disp3dcnt = 0x0001;

        // Set palette[0] (backdrop) to black explicitly.
        nds.shared.palette[0] = 0;
        nds.shared.palette[1] = 0;

        // Submit a single screen-covering red triangle directly to the
        // geometry buffer (skipping the GX command path here since
        // Phase 6 already tested that). swap_buffers will move it to
        // raster_polygons + rasterize. This validates the
        // rasterize→Engine-A-BG0→top-framebuffer pipeline end to end.
        use crate::gpu3d::viewport::{ScreenPolygon, ScreenVertex};
        nds.shared.gpu3d.geometry_polygons.push(ScreenPolygon {
            vertices: vec![
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 50 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 200 << 8,
                    screen_y: 50 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 125 << 8,
                    screen_y: 150 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
            ],
            attr: (0x1F << 16) | (1 << 6) | (1 << 7), // opaque, render front/back
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        });
        nds.shared.gpu3d.swap_pending = true;
        // Rasterize directly so the framebuffer is populated before any
        // scanlines render. In a real run, this happens at VBlank-end of
        // the previous frame (line 0 transition). Tests don't need to
        // wait an extra frame for that.
        nds.shared.gpu3d.swap_buffers(None);

        nds.cpu9.halted = true;
        nds.cpu7.halted = true;
        nds.run_frame();

        // Top framebuffer center should now be red — the 3D rasterizer
        // produced a red pixel, Engine A's BG0 sourced it, the compositor
        // wrote it through to the top framebuffer.
        let center_idx = 100 * SCREEN_WIDTH + 125;
        let c = nds.framebuffer_top[center_idx];
        let r = c & 0x1F;
        assert!(
            r >= 30,
            "top framebuffer center should be red, got 0x{:04X} (r={})",
            c,
            r
        );

        // A pixel far outside the triangle should be backdrop (black).
        let outside_idx = 10 * SCREEN_WIDTH + 10;
        assert_eq!(
            nds.framebuffer_top[outside_idx] & 0x7FFF,
            0,
            "top framebuffer corner should be backdrop black"
        );
    }

    #[test]
    fn test_3d_disabled_when_bg0_3d_clear() {
        let mut nds = Nds::new(None, None);

        // Engine A: BG0 enabled but BG0_3D *not* set. Plain BG0 source.
        nds.shared.engine_a.dispcnt = 0x0001_0100;
        nds.shared.gpu3d.rasterizer.disp3dcnt = 0x0001;

        // Push a red triangle into the geometry buffer so swap_buffers picks it up.
        use crate::gpu3d::viewport::{ScreenPolygon, ScreenVertex};
        nds.shared.gpu3d.geometry_polygons.push(ScreenPolygon {
            vertices: vec![
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 50 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 200 << 8,
                    screen_y: 50 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 125 << 8,
                    screen_y: 150 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
            ],
            attr: (0x1F << 16) | (1 << 6) | (1 << 7),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        });
        nds.shared.gpu3d.swap_pending = true;
        nds.shared.gpu3d.swap_buffers(None);

        nds.cpu9.halted = true;
        nds.cpu7.halted = true;
        nds.run_frame();

        // BG0 is now showing tile data (which is all zeros), so the center
        // pixel should NOT be red.
        let center_idx = 100 * SCREEN_WIDTH + 125;
        let c = nds.framebuffer_top[center_idx];
        let r = c & 0x1F;
        assert!(
            r < 5,
            "BG0_3D disabled: should not see 3D pixels in framebuffer; got 0x{:04X}",
            c
        );
    }

    #[test]
    fn test_3d_pipeline_via_arm9_io_writes() {
        let mut nds = Nds::new(None, None);
        {
            let mut bus = Bus9::new(
                &mut nds.mem9,
                &mut nds.shared,
                nds.cpu9.cp15.itcm,
                nds.cpu9.cp15.dtcm,
            );

            // BEGIN_VTXS triangles (cmd 0x40, 1 param). Direct port at
            // 0x0400_0440 + (0x40 - 0x10) * 4 = 0x0400_0500.
            bus.write32(0x0400_04A4, (1 << 13) | (0x1F << 16) | (1 << 6) | (1 << 7));
            bus.write32(0x0400_0500, 0);

            // VTX_16 (cmd 0x23, 2 params). Direct port at 0x0400_048C.
            let z_half = 0x800u32; // 0.5 in 1.19.12
            for _ in 0..3 {
                bus.write32(0x0400_048C, 0);
                bus.write32(0x0400_048C, z_half);
            }

            // SWAP_BUFFERS (cmd 0x50). Direct port at 0x0400_0540.
            bus.write32(0x0400_0540, 0);
        }

        assert_eq!(
            nds.shared.gpu3d.geometry_polygons.len(),
            1,
            "one triangle should have landed in geometry buffer"
        );
        assert!(nds.shared.gpu3d.swap_pending);

        // Run a frame so VBlank-end swaps the buffers.
        nds.cpu9.halted = true;
        nds.cpu7.halted = true;
        nds.run_frame();
        assert!(!nds.shared.gpu3d.swap_pending);
        assert_eq!(nds.shared.gpu3d.raster_polygons.len(), 1);
        assert!(nds.shared.gpu3d.geometry_polygons.is_empty());
    }

    #[test]
    fn test_out_of_list_vtx_does_not_seed_inherited_position_via_io() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write32(0x0400_048C, (7u32 << 12) | ((7u32 << 12) << 16));
        bus.write32(0x0400_048C, 7u32 << 12);
        assert_eq!(bus.shared.gpu3d.vertex.last_pos, [0, 0, 0]);

        bus.write32(0x0400_0500, 0); // BEGIN_VTXS triangles
        bus.write32(0x0400_0494, 0); // VTX_XY inherits Z from last_pos

        assert_eq!(bus.shared.gpu3d.vertex.last_pos, [0, 0, 0]);
        assert_eq!(bus.shared.gpu3d.vertex.vertex_buffer[0].clip[2], 0);
    }

    #[test]
    fn test_gxfifo_packed_port_mirror_accepts_commands() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write32(0x0400_0404, 0x0000_0010); // MTX_MODE via GXFIFO mirror.
        bus.write32(0x0400_0404, 1);
        drop(bus);

        assert!(matches!(
            nds.shared.gpu3d.stacks.mode,
            gpu3d::stacks::MtxMode::Position
        ));
    }

    #[test]
    fn test_gxfifo_dma_transfers_112_words_per_trigger() {
        let mut nds = Nds::new(None, None);
        for i in 0..120u32 {
            let off = (i * 4) as usize;
            nds.shared.main_ram[off..off + 4].copy_from_slice(&0x0000_0011u32.to_le_bytes());
        }
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write32(0x0400_00D4, 0x0200_0000);
        bus.write32(0x0400_00D8, 0x0400_0400);
        bus.write32(
            0x0400_00DC,
            (1 << 31) | (7 << 27) | (1 << 26) | (2 << 21) | 120,
        );

        assert!(bus.shared.dma9.channels[3].active);
        assert_eq!(bus.shared.dma9.channels[3].internal_count, 120);

        let irq = bus.run_dma(3);

        assert!(!irq);
        assert!(bus.shared.dma9.channels[3].active);
        assert_ne!(bus.shared.dma9.channels[3].control & (1 << 31), 0);
        assert_eq!(bus.shared.dma9.channels[3].internal_count, 8);
        assert_eq!(bus.shared.dma9.channels[3].internal_sad, 0x0200_01C0);

        let irq = bus.run_dma(3);

        assert!(!irq);
        assert!(!bus.shared.dma9.channels[3].active);
        assert_eq!(bus.shared.dma9.channels[3].control & (1 << 31), 0);
        assert_eq!(bus.shared.dma9.channels[3].internal_count, 0);
        assert_eq!(bus.shared.dma9.channels[3].internal_sad, 0x0200_01E0);
    }

    #[test]
    fn test_gxfifo_dma_trigger_continues_while_fifo_below_half() {
        let mut nds = Nds::new(None, None);
        for i in 0..120u32 {
            let off = (i * 4) as usize;
            nds.shared.main_ram[off..off + 4].copy_from_slice(&0x0000_0011u32.to_le_bytes());
        }
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write32(0x0400_00D4, 0x0200_0000);
        bus.write32(0x0400_00D8, 0x0400_0400);
        bus.write32(
            0x0400_00DC,
            (1 << 31) | (7 << 27) | (1 << 26) | (2 << 21) | 120,
        );

        bus.write32(0x0400_0400, 0x0000_0011);

        assert!(!bus.shared.dma9.channels[3].active);
        assert_eq!(bus.shared.dma9.channels[3].control & (1 << 31), 0);
        assert_eq!(bus.shared.dma9.channels[3].internal_count, 0);
        assert_eq!(bus.shared.dma9.channels[3].internal_sad, 0x0200_01E0);
    }

    #[test]
    fn test_gxstat_low_reflects_idle_geometry_at_boot() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );
        // GXSTAT low half: test busy/result, stack pointers, stack busy,
        // overflow. At boot those should all be clear.
        let stat = bus.read16(0x0400_0600);
        assert_eq!(stat, 0);
    }

    #[test]
    fn test_gxstat_high_exposes_real_fifo_status_bits() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        let stat = bus.read32(0x0400_0600);

        assert_ne!(stat & (1 << 25), 0, "FIFO less-than-half bit");
        assert_ne!(stat & (1 << 26), 0, "FIFO empty bit");
    }

    #[test]
    fn test_gxstat_less_than_half_irq_requests_gxfifo_irq() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write32(0x0400_0600, 1 << 30);

        assert_eq!(bus.read32(0x0400_0600) & (3 << 30), 1 << 30);
        drop(bus);
        assert_ne!(nds.shared.irq9.read_if() & interrupt::Irq::GxFifo.bit(), 0);
    }

    #[test]
    fn test_gx_ram_count_reports_geometry_buffer() {
        let mut nds = Nds::new(None, None);
        use crate::gpu3d::viewport::{ScreenPolygon, ScreenVertex};
        nds.shared.gpu3d.geometry_polygons.push(ScreenPolygon {
            vertices: vec![
                ScreenVertex {
                    screen_x: 0,
                    screen_y: 0,
                    depth_z: 0,
                    w: 4096,
                    color: 0,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 1,
                    screen_y: 0,
                    depth_z: 0,
                    w: 4096,
                    color: 0,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 0,
                    screen_y: 1,
                    depth_z: 0,
                    w: 4096,
                    color: 0,
                    tex: [0, 0],
                },
            ],
            attr: (0x1F << 16) | (1 << 6) | (1 << 7),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        });
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        let count = bus.read32(0x0400_0604);

        assert_eq!(count & 0x0FFF, 1);
        assert_eq!((count >> 16) & 0x1FFF, 3);
    }

    #[test]
    fn test_disp_1dot_depth_register_round_trip() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write16(0x0400_0610, 0xFFFF);

        assert_eq!(bus.read16(0x0400_0610), 0x7FFF);
        assert_eq!(bus.read32(0x0400_0610), 0x7FFF);
    }

    #[test]
    fn test_gx_readable_clip_matrix_exposes_current_transform() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write32(0x0400_0440, 1); // MTX_MODE position
        bus.write32(0x0400_0470, 2 << 12);
        bus.write32(0x0400_0470, 3 << 12);
        bus.write32(0x0400_0470, 4 << 12);

        assert_eq!(bus.read32(0x0400_0670), (2u32 << 12));
        assert_eq!(bus.read32(0x0400_0674), (3u32 << 12));
        assert_eq!(bus.read32(0x0400_0678), (4u32 << 12));
    }

    #[test]
    fn test_pos_test_writes_result_registers() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write32(0x0400_05C4, (2u32 << 12) | ((3u32 << 12) << 16));
        bus.write32(0x0400_05C4, 4u32 << 12);

        assert_eq!(bus.read32(0x0400_0620), 2u32 << 12);
        assert_eq!(bus.read32(0x0400_0624), 3u32 << 12);
        assert_eq!(bus.read32(0x0400_0628), 4u32 << 12);
        assert_eq!(bus.read32(0x0400_062C), 1u32 << 12);
    }

    #[test]
    fn test_geometry_result_registers_support_halfword_reads() {
        let mut nds = Nds::new(None, None);
        nds.shared.gpu3d.stacks.vector.m[0] = 0x1234_5678;
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write32(0x0400_05C4, 0x5678_1234);
        bus.write32(0x0400_05C4, 0);

        assert_eq!(bus.read16(0x0400_0620), 0x1234);
        assert_eq!(bus.read16(0x0400_0622), 0x0000);
        assert_eq!(bus.read16(0x0400_0624), 0x5678);

        assert_eq!(bus.read16(0x0400_0640), 0x1000);
        assert_eq!(bus.read16(0x0400_0642), 0x0000);

        assert_eq!(bus.read16(0x0400_0680), 0x5678);
        assert_eq!(bus.read16(0x0400_0682), 0x1234);
    }

    #[test]
    fn test_vec_test_writes_direction_result_registers() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write32(0x0400_05C8, 1);

        assert_eq!(bus.read16(0x0400_0630), 8);
        assert_eq!(bus.read16(0x0400_0632), 0);
        assert_eq!(bus.read16(0x0400_0634), 0);
    }

    #[test]
    fn test_arm9_fog_table_halfword_writes_are_contiguous() {
        let mut nds = Nds::new(None, None);
        {
            let mut bus = Bus9::new(
                &mut nds.mem9,
                &mut nds.shared,
                nds.cpu9.cp15.itcm,
                nds.cpu9.cp15.dtcm,
            );

            bus.write16(0x0400_0360, 0x2211);
            bus.write16(0x0400_0362, 0x4433);
        }

        assert_eq!(
            &nds.shared.gpu3d.rasterizer.fog_table[0..4],
            &[0x11, 0x22, 0x33, 0x44]
        );
    }

    #[test]
    fn test_box_test_sets_gxstat_visible_bit() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            nds.cpu9.cp15.itcm,
            nds.cpu9.cp15.dtcm,
        );

        bus.write32(0x0400_05C0, 0);
        bus.write32(0x0400_05C0, 0);
        bus.write32(0x0400_05C0, 0);

        assert_ne!(bus.read16(0x0400_0600) & (1 << 1), 0);
        assert_eq!(
            bus.read16(0x0400_0600) & 1,
            0,
            "test busy should clear immediately in HLE"
        );
    }

    #[test]
    fn test_set_touch_drives_tsc_and_extkeyin() {
        let mut nds = Nds::new(None, None);
        // Pen down at (128, 96).
        nds.set_touch(128, 96, true);
        // EXTKEYIN bit 6 = 0 when pen down (active-low).
        assert_eq!(nds.shared.extkeyin & (1 << 6), 0);
        // Reading X via TSC over the SPI bus should give a non-trivial ADC.
        // Drive a 3-byte conversion against device 2 (TSC). CS must be held
        // for the first two bytes; released on the third (final).
        let bus = &mut nds.shared.spi;
        let cnt_hold = (1 << 15) | (2 << 8) | (1 << 11);
        let cnt_drop = (1 << 15) | (2 << 8);
        bus.cnt = cnt_hold;
        let _ = bus.write_data(0x80 | (5 << 4)); // control byte
        bus.cnt = cnt_hold;
        let _ = bus.write_data(0);
        let hi = bus.read_data();
        bus.cnt = cnt_drop;
        let _ = bus.write_data(0);
        let lo = bus.read_data();
        let adc = ((hi as u16) << 5) | ((lo as u16) >> 3);
        // 128 of 255 → roughly halfway between ADC_X1 (0x0200) and ADC_X2
        // (0x0E00); expect ≈ 0x0800 ± 32.
        assert!(
            (0x07E0..=0x0820).contains(&adc),
            "TSC X ADC {:#06X} should be near 0x0800",
            adc
        );

        nds.set_touch(0, 0, false);
        assert_eq!(nds.shared.extkeyin & (1 << 6), 1 << 6);
    }

    #[test]
    fn test_set_backup_kind_and_save_round_trip() {
        let mut nds = Nds::new(None, None);
        nds.set_backup_kind(cart::BackupKind::Eeprom8K);

        // Drive AUXSPI: WRITE_ENABLE, then WRITE 4 bytes at addr 0x100.
        let aux = &mut nds.shared.auxspi;
        let base_cnt = (1 << 15) | (1 << 13);
        // WRITE_ENABLE (single byte, hold off)
        aux.cnt = base_cnt;
        let _ = aux.write_data(0x06);
        // WRITE cmd 0x02 + 2-byte addr 0x0100 + 4 data bytes.
        let mut send = |aux: &mut cart::AuxSpi, byte: u8, hold: bool| {
            aux.cnt = base_cnt | if hold { 1 << 6 } else { 0 };
            let _ = aux.write_data(byte);
        };
        send(aux, 0x02, true);
        send(aux, 0x01, true);
        send(aux, 0x00, true);
        send(aux, 0xAA, true);
        send(aux, 0xBB, true);
        send(aux, 0xCC, true);
        send(aux, 0xDD, false);

        let sav = nds.export_save().expect("save");
        assert_eq!(sav.len(), 8 * 1024);
        assert_eq!(&sav[0x100..0x104], &[0xAA, 0xBB, 0xCC, 0xDD]);

        // Import into a fresh Nds, read back via AUXSPI.
        let mut nds2 = Nds::new(None, None);
        nds2.set_backup_kind(cart::BackupKind::Eeprom8K);
        nds2.import_save(&sav);
        let aux = &mut nds2.shared.auxspi;
        let mut send = |aux: &mut cart::AuxSpi, byte: u8, hold: bool| -> u8 {
            aux.cnt = base_cnt | if hold { 1 << 6 } else { 0 };
            let _ = aux.write_data(byte);
            aux.read_data()
        };
        send(aux, 0x03, true);
        send(aux, 0x01, true);
        send(aux, 0x00, true);
        let mut out = [0u8; 4];
        for i in 0..4 {
            let hold = i + 1 < 4;
            out[i] = send(aux, 0, hold);
        }
        assert_eq!(out, [0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn test_arm9_auxspi_registers_route_to_backup() {
        let mut nds = Nds::new(None, None);
        nds.set_backup_kind(cart::BackupKind::Eeprom8K);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            cpu::cp15::TcmRegion::disabled(),
            cpu::cp15::TcmRegion::disabled(),
        );
        let base_cnt = (1 << 15) | (1 << 13);
        let send = |bus: &mut Bus9<'_>, byte: u8, hold: bool| -> u8 {
            bus.write16(0x0400_01A0, base_cnt | if hold { 1 << 6 } else { 0 });
            bus.write16(0x0400_01A2, byte as u16);
            bus.read16(0x0400_01A2) as u8
        };

        send(&mut bus, 0x06, false);
        send(&mut bus, 0x02, true);
        send(&mut bus, 0x01, true);
        send(&mut bus, 0x00, true);
        send(&mut bus, 0x5A, false);

        send(&mut bus, 0x03, true);
        send(&mut bus, 0x01, true);
        send(&mut bus, 0x00, true);
        assert_eq!(send(&mut bus, 0, false), 0x5A);
    }

    #[test]
    fn test_arm9_slot1_command_registers_preserve_byte_order() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            cpu::cp15::TcmRegion::disabled(),
            cpu::cp15::TcmRegion::disabled(),
        );

        bus.write32(0x0400_01A8, 0xDDCC_BBAA);
        bus.write32(0x0400_01AC, 0x4433_2211);

        assert_eq!(
            nds.shared.slot1_command,
            [0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44]
        );
    }

    #[test]
    fn test_arm9_slot1_header_read_queues_card_data() {
        let mut nds = Nds::new(None, None);
        nds.shared.slot1_rom = (0..=0x1FF).map(|v| v as u8).collect();
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            cpu::cp15::TcmRegion::disabled(),
            cpu::cp15::TcmRegion::disabled(),
        );

        bus.write8(0x0400_01A8, 0x00);
        bus.write32(0x0400_01A4, (1 << 31) | (7 << 24));

        assert_eq!(
            bus.read32(0x0400_01A4) & ((1 << 31) | (1 << 23)),
            (1 << 31) | (1 << 23)
        );
        assert_eq!(bus.read32(0x0410_0010), 0x0302_0100);
        assert_eq!(bus.read32(0x0400_01A4) & ((1 << 31) | (1 << 23)), 0);
    }

    #[test]
    fn test_arm9_slot1_b7_read_uses_big_endian_command_offset() {
        let mut nds = Nds::new(None, None);
        nds.shared.slot1_rom = (0..=0x3F).map(|v| v as u8).collect();
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            cpu::cp15::TcmRegion::disabled(),
            cpu::cp15::TcmRegion::disabled(),
        );

        for (i, byte) in [0xB7, 0x00, 0x00, 0x00, 0x04, 0, 0, 0].iter().enumerate() {
            bus.write8(0x0400_01A8 + i as u32, *byte);
        }
        bus.write32(0x0400_01A4, (1 << 31) | (7 << 24));

        assert_eq!(bus.read32(0x0410_0010), 0x0706_0504);
        assert_eq!(bus.read32(0x0410_0010), 0xFFFF_FFFF);
    }

    #[test]
    fn test_arm9_slot1_data_port_repeats_for_incrementing_word_reads() {
        let mut nds = Nds::new(None, None);
        nds.shared.slot1_rom = (0..=0x1F).map(|v| v as u8).collect();
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            cpu::cp15::TcmRegion::disabled(),
            cpu::cp15::TcmRegion::disabled(),
        );

        bus.write8(0x0400_01A8, 0x00);
        bus.write32(0x0400_01A4, (1 << 31) | (1 << 24));

        assert_eq!(bus.read32(0x0410_0010), 0x0302_0100);
        assert_eq!(bus.read32(0x0410_0014), 0x0706_0504);
        assert_eq!(bus.read32(0x0410_0018), 0x0B0A_0908);
        assert_eq!(bus.read32(0x0410_001C), 0x0F0E_0D0C);
    }

    #[test]
    fn test_arm9_slot1_status_polling_completes_unread_transfer() {
        let mut nds = Nds::new(None, None);
        nds.shared.slot1_rom = (0..=0x1FF).map(|v| v as u8).collect();
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            cpu::cp15::TcmRegion::disabled(),
            cpu::cp15::TcmRegion::disabled(),
        );

        bus.write8(0x0400_01A8, 0x00);
        bus.write32(0x0400_01A4, (1 << 31) | (1 << 24));
        assert_eq!(bus.read32(0x0410_0010), 0x0302_0100);

        let mut status = 0;
        for _ in 0..8 {
            status = bus.read32(0x0400_01A4);
        }

        assert_eq!(status & ((1 << 31) | (1 << 23)), 0);
        assert_eq!(bus.read32(0x0410_0010), 0xFFFF_FFFF);
    }

    #[test]
    fn test_arm9_slot1_read_with_irq_enable_requests_slot1_data_irq() {
        let mut nds = Nds::new(None, None);
        nds.shared.slot1_rom = (0..=0x1F).map(|v| v as u8).collect();
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            cpu::cp15::TcmRegion::disabled(),
            cpu::cp15::TcmRegion::disabled(),
        );

        bus.write8(0x0400_01A8, 0x00);
        bus.write32(0x0400_01A4, (1 << 31) | (7 << 24) | (1 << 14));

        assert_ne!(
            nds.shared.irq9.read_if() & interrupt::Irq::Slot1Data.bit(),
            0
        );
    }

    #[test]
    fn test_arm9_slot1_transfer_fires_slot1_dma() {
        let mut nds = Nds::new(None, None);
        nds.shared.slot1_rom = (0..=0x1F).map(|v| v as u8).collect();
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            cpu::cp15::TcmRegion::disabled(),
            cpu::cp15::TcmRegion::disabled(),
        );

        bus.write8(0x0400_01A8, 0x00);
        bus.write32(0x0400_00B0, 0x0410_0010);
        bus.write32(0x0400_00B4, 0x0200_1000);
        bus.write32(
            0x0400_00B8,
            (1u32 << 31) | (1 << 26) | (2 << 23) | (5 << 27) | 2,
        );
        bus.write32(0x0400_01A4, (1 << 31) | (1 << 24));

        assert_eq!(
            &nds.shared.main_ram[0x1000..0x1008],
            &[0, 1, 2, 3, 4, 5, 6, 7]
        );
    }

    #[test]
    fn test_arm9_slot1_dma_fires_when_armed_after_card_data_ready() {
        let mut nds = Nds::new(None, None);
        nds.shared.slot1_rom = (0..=0x1F).map(|v| v as u8).collect();
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            cpu::cp15::TcmRegion::disabled(),
            cpu::cp15::TcmRegion::disabled(),
        );

        bus.write8(0x0400_01A8, 0x00);
        bus.write32(0x0400_01A4, (1 << 31) | (1 << 24));
        bus.write32(0x0400_00BC, 0x0410_0010);
        bus.write32(0x0400_00C0, 0x0200_2000);
        bus.write32(
            0x0400_00C4,
            (1u32 << 31) | (1 << 26) | (2 << 23) | (5 << 27) | 4,
        );

        assert_eq!(
            &nds.shared.main_ram[0x2000..0x2010],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
        assert_eq!(nds.shared.slot1_data.len(), 124);
        assert_eq!(nds.shared.dma9.read_control(1) & (1 << 31), 0);
    }

    #[test]
    fn test_exmemcnt_defaults_slot1_to_arm7_and_is_writable_by_arm9() {
        let mut nds = Nds::new(None, None);
        let mut bus = Bus9::new(
            &mut nds.mem9,
            &mut nds.shared,
            cpu::cp15::TcmRegion::disabled(),
            cpu::cp15::TcmRegion::disabled(),
        );

        assert_eq!(bus.read16(0x0400_0204) & (1 << 11), 1 << 11);
        bus.write16(0x0400_0204, 0);
        assert_eq!(bus.read16(0x0400_0204) & (1 << 11), 0);
    }

    #[test]
    fn test_firmware_nickname_via_spi_io() {
        // Drive SPI from the ARM7 bus to confirm I/O wiring routes correctly.
        let mut nds = Nds::new(None, None);
        let mut bus = Bus7::new(&mut nds.mem7, &mut nds.shared);
        // Enable SPI + select firmware (device 1).
        bus.write16(0x0400_01C0, (1 << 15) | (1 << 8));
        // Issue READ cmd (0x03) at addr 0x3FE06 — the nickname start.
        let cnt_hold = (1 << 15) | (1 << 8) | (1 << 11); // hold = bit 11
        bus.write16(0x0400_01C0, cnt_hold);
        bus.write16(0x0400_01C2, 0x03); // READ
        bus.write16(0x0400_01C2, 0x03); // addr [23:16]
        bus.write16(0x0400_01C2, 0xFE); // addr [15:8]
        bus.write16(0x0400_01C2, 0x06); // addr [7:0]
                                        // Read 4 bytes — first should be 'N'.
        bus.write16(0x0400_01C2, 0);
        let n = bus.read16(0x0400_01C2) as u8;
        bus.write16(0x0400_01C2, 0);
        let _hi = bus.read16(0x0400_01C2);
        bus.write16(0x0400_01C2, 0);
        let d = bus.read16(0x0400_01C2) as u8;
        // Final byte — release hold.
        bus.write16(0x0400_01C0, (1 << 15) | (1 << 8));
        bus.write16(0x0400_01C2, 0);
        let _s_hi = bus.read16(0x0400_01C2);

        assert_eq!(n, b'N');
        assert_eq!(d, b'D');
    }

    #[test]
    fn test_keypad_irq_or_mode() {
        let mut nds = Nds::new(None, None);
        // KEYCNT9: enable (bit 14) + OR mode (bit 15 = 0) + watch A (bit 0)
        nds.shared.keycnt9 = (1 << 14) | (1 << 0);
        // Press A (bit 0 = 0)
        nds.set_keys(0x03FE);
        assert!(nds.shared.irq9.read_if() & Irq::Keypad.bit() != 0);
    }

    #[test]
    fn test_keypad_irq_and_mode_requires_all() {
        let mut nds = Nds::new(None, None);
        // KEYCNT9: enable + AND mode (bit 15 = 1) + watch A AND B (bits 0+1)
        nds.shared.keycnt9 = (1 << 14) | (1 << 15) | (1 << 0) | (1 << 1);
        // Press only A → no IRQ in AND mode
        nds.set_keys(0x03FE);
        assert_eq!(nds.shared.irq9.read_if() & Irq::Keypad.bit(), 0);
        // Press both A and B → IRQ
        nds.set_keys(0x03FC);
        assert!(nds.shared.irq9.read_if() & Irq::Keypad.bit() != 0);
    }

    #[test]
    fn test_swi_div_via_hle() {
        let mut nds = Nds::new(None, None);
        nds.direct_boot = true;
        nds.cpu9.cpsr = Psr::new(CpuMode::System);
        nds.cpu9.cpsr.bits &= !(1 << 7);

        // Plant SWI 0x09 (Div) at 0x02000000 + B . at 0x02000004.
        // NDS/GBA convention: SWI number lives in bits 23:16 of the
        // immediate, so `SWI 0x09` encodes as 0xEF09_0000.
        let swi = 0xEF09_0000u32;
        let bself = 0xEAFF_FFFEu32;
        nds.shared.main_ram[0..4].copy_from_slice(&swi.to_le_bytes());
        nds.shared.main_ram[4..8].copy_from_slice(&bself.to_le_bytes());

        nds.cpu9.regs[0] = 100;
        nds.cpu9.regs[1] = 7;
        nds.cpu9.regs[15] = 0x0200_0000;
        nds.cpu9.pipeline_flushed = true;

        // Step the SWI instruction
        nds.step_one();

        assert_eq!(nds.cpu9.regs[0] as i32, 14);
        assert_eq!(nds.cpu9.regs[1] as i32, 2);
    }

    #[test]
    fn test_direct_boot_unhandled_arm9_swi_returns_without_bios_vector() {
        let mut nds = Nds::new(None, None);
        nds.direct_boot = true;
        nds.cpu9.cpsr = Psr::new(CpuMode::System);
        nds.cpu9.regs[15] = 0x0200_1234;

        nds.handle_swi9(0x99);

        assert_eq!(nds.cpu9.cpsr.mode(), CpuMode::System);
        assert_eq!(nds.cpu9.regs[15], 0x0200_1234);
    }

    #[test]
    fn test_direct_boot_unhandled_arm7_swi_returns_without_bios_vector() {
        let mut nds = Nds::new(None, None);
        nds.direct_boot = true;
        nds.cpu7.cpsr = Psr::new(CpuMode::System);
        nds.cpu7.regs[15] = 0x037F_C648;

        nds.handle_swi7(0x99);

        assert_eq!(nds.cpu7.cpsr.mode(), CpuMode::System);
        assert_eq!(nds.cpu7.regs[15], 0x037F_C648);
    }
}

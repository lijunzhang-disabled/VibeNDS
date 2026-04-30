//! DMA — 4 channels per CPU.
//!
//! ARM9 has 3-bit start mode (Immediate / VBlank / HBlank-visible / Display
//! sync / Main-mem display FIFO / Slot1 / Slot2 / GXFIFO).
//! ARM7 has 2-bit start mode (Immediate / VBlank / Slot1 / channel-specific
//! sound or wireless on channels 1-3).
//!
//! Phase 4 implements: Immediate, VBlank, HBlank-visible (ARM9) and the
//! address-control / repeat / IRQ-on-complete machinery. Slot1/Slot2/GXFIFO
//! triggers and ARM7 sound DMA are wired in their respective phases.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DmaTiming {
    Immediate,
    VBlank,
    /// HBlank, visible scanlines only (ARM9). Not supported by ARM7.
    HBlankVisible,
    /// Display sync — start of HDraw, lines 2..193 (ARM9 only).
    DisplaySync,
    /// Main memory display FIFO (ARM9 channel 3 only).
    MainMemDisplayFifo,
    /// NDS-cart slot-1 ready.
    Slot1,
    /// GBA-cart slot-2 ready.
    Slot2,
    /// GXFIFO half-empty (ARM9 only) — kicks geometry pipeline DMA.
    GxFifo,
    /// Channel-specific (ARM7 only): sound channel feed on ch1-3,
    /// wireless on ch0.
    Special,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AddrControl {
    Increment,
    Decrement,
    Fixed,
    /// Increment with reload at the end of each block (dest only).
    IncrementReload,
}

impl AddrControl {
    pub fn from_bits(bits: u32) -> Self {
        match bits & 0x3 {
            0 => AddrControl::Increment,
            1 => AddrControl::Decrement,
            2 => AddrControl::Fixed,
            3 => AddrControl::IncrementReload,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DmaChannel {
    /// SAD register (programmed value).
    pub sad: u32,
    /// DAD register.
    pub dad: u32,
    /// Word count (programmed). 0 means max for the channel.
    pub count_programmed: u32,
    /// Control register (DMAxCNT).
    pub control: u32,

    /// Internal addresses captured on enable rising edge.
    pub internal_sad: u32,
    pub internal_dad: u32,
    pub internal_count: u32,

    /// True when the channel has been started but is waiting for its
    /// trigger event (VBlank, HBlank, etc.).
    pub active: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DmaController {
    pub channels: [DmaChannel; 4],
    /// True for ARM9 (3-bit start-mode field), false for ARM7.
    pub is_arm9: bool,
}

impl DmaController {
    pub fn new(is_arm9: bool) -> Self {
        let mut c = Self::default();
        c.is_arm9 = is_arm9;
        c
    }

    /// Maximum word count for channel `id` when `count_programmed = 0`.
    /// ARM9: 0x20_0000 words on all channels. ARM7: 0x4000 (DMA0-2),
    /// 0x10000 (DMA3).
    pub fn max_count(&self, id: usize) -> u32 {
        if self.is_arm9 {
            0x20_0000
        } else if id == 3 {
            0x10000
        } else {
            0x4000
        }
    }

    pub fn read_sad(&self, id: usize) -> u32 { self.channels[id].sad }
    pub fn read_dad(&self, id: usize) -> u32 { self.channels[id].dad }
    pub fn read_count(&self, id: usize) -> u32 { self.channels[id].count_programmed }
    pub fn read_control(&self, id: usize) -> u32 { self.channels[id].control }

    pub fn write_sad(&mut self, id: usize, val: u32) {
        // Bit width depends on channel + side; we store full 32 bits and
        // mask on use.
        self.channels[id].sad = val;
    }
    pub fn write_dad(&mut self, id: usize, val: u32) {
        self.channels[id].dad = val;
    }
    pub fn write_count(&mut self, id: usize, val: u32) {
        self.channels[id].count_programmed = val;
    }

    /// Write the control register. Returns the timing class that the
    /// channel is now armed for (or None if disabled), and whether the
    /// caller should run an immediate transfer.
    pub fn write_control(&mut self, id: usize, val: u32) -> WriteControlEffect {
        let was_enabled = self.channels[id].control & (1 << 31) != 0;
        self.channels[id].control = val;
        let now_enabled = val & (1 << 31) != 0;

        if !was_enabled && now_enabled {
            // 0→1 enable: latch internal addresses and count.
            let count = self.channels[id].count_programmed;
            let max = self.max_count(id);
            let internal_count = if count == 0 { max } else { count.min(max) };

            self.channels[id].internal_sad = self.channels[id].sad & 0x0FFF_FFFF;
            self.channels[id].internal_dad = self.channels[id].dad & 0x0FFF_FFFF;
            self.channels[id].internal_count = internal_count;
            self.channels[id].active = true;

            let timing = self.timing(id);
            return match timing {
                DmaTiming::Immediate => WriteControlEffect::RunNow,
                _ => WriteControlEffect::Armed(timing),
            };
        } else if was_enabled && !now_enabled {
            self.channels[id].active = false;
        }
        WriteControlEffect::Idle
    }

    pub fn timing(&self, id: usize) -> DmaTiming {
        let bits = (self.channels[id].control >> 27) & 0x7;
        if self.is_arm9 {
            match bits {
                0 => DmaTiming::Immediate,
                1 => DmaTiming::VBlank,
                2 => DmaTiming::HBlankVisible,
                3 => DmaTiming::DisplaySync,
                4 => DmaTiming::MainMemDisplayFifo,
                5 => DmaTiming::Slot1,
                6 => DmaTiming::Slot2,
                7 => DmaTiming::GxFifo,
                _ => unreachable!(),
            }
        } else {
            // ARM7 only uses 2 bits.
            match bits & 0x3 {
                0 => DmaTiming::Immediate,
                1 => DmaTiming::VBlank,
                2 => DmaTiming::Slot1,
                3 => DmaTiming::Special,
                _ => unreachable!(),
            }
        }
    }

    pub fn dst_control(&self, id: usize) -> AddrControl {
        AddrControl::from_bits(self.channels[id].control >> 21)
    }

    pub fn src_control(&self, id: usize) -> AddrControl {
        AddrControl::from_bits(self.channels[id].control >> 23)
    }

    pub fn word_size(&self, id: usize) -> u32 {
        if self.channels[id].control & (1 << 26) != 0 { 4 } else { 2 }
    }

    pub fn repeat(&self, id: usize) -> bool {
        self.channels[id].control & (1 << 25) != 0
    }

    pub fn irq_on_complete(&self, id: usize) -> bool {
        self.channels[id].control & (1 << 30) != 0
    }

    /// Channels armed for `timing`, in priority order (ascending channel id).
    pub fn channels_for_timing(&self, timing: DmaTiming) -> Vec<usize> {
        (0..4).filter(|&i| self.channels[i].active && self.timing(i) == timing).collect()
    }

    /// Apply post-transfer state: clear active + enable bit (one-shot) or
    /// reload count (repeat).
    pub fn finish_transfer(&mut self, id: usize) {
        if self.repeat(id) {
            // Reload count for next trigger.
            let count = self.channels[id].count_programmed;
            let max = self.max_count(id);
            self.channels[id].internal_count = if count == 0 { max } else { count.min(max) };
            // Optionally reload dest if AddrControl::IncrementReload.
            if self.dst_control(id) == AddrControl::IncrementReload {
                self.channels[id].internal_dad = self.channels[id].dad & 0x0FFF_FFFF;
            }
        } else {
            self.channels[id].active = false;
            self.channels[id].control &= !(1 << 31);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum WriteControlEffect {
    Idle,
    Armed(DmaTiming),
    RunNow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arm9_immediate_arm() {
        let mut d = DmaController::new(true);
        d.write_sad(0, 0x0200_0000);
        d.write_dad(0, 0x0200_1000);
        d.write_count(0, 16);
        let eff = d.write_control(0, 1 << 31);
        assert!(matches!(eff, WriteControlEffect::RunNow));
        assert_eq!(d.channels[0].internal_count, 16);
    }

    #[test]
    fn test_arm9_vblank_arms_only() {
        let mut d = DmaController::new(true);
        d.write_count(0, 4);
        let eff = d.write_control(0, (1 << 31) | (1 << 27));
        assert!(matches!(eff, WriteControlEffect::Armed(DmaTiming::VBlank)));
    }

    #[test]
    fn test_arm9_count_zero_means_max() {
        let mut d = DmaController::new(true);
        d.write_count(0, 0);
        d.write_control(0, 1 << 31);
        assert_eq!(d.channels[0].internal_count, 0x20_0000);
    }

    #[test]
    fn test_arm7_count_zero_means_4000() {
        let mut d = DmaController::new(false);
        d.write_count(0, 0);
        d.write_control(0, 1 << 31);
        assert_eq!(d.channels[0].internal_count, 0x4000);
    }

    #[test]
    fn test_arm7_dma3_count_zero_means_10000() {
        let mut d = DmaController::new(false);
        d.write_count(3, 0);
        d.write_control(3, 1 << 31);
        assert_eq!(d.channels[3].internal_count, 0x10000);
    }

    #[test]
    fn test_arm9_timing_modes() {
        let d = DmaController::new(true);
        for (bits, expected) in [
            (0, DmaTiming::Immediate),
            (1, DmaTiming::VBlank),
            (2, DmaTiming::HBlankVisible),
            (3, DmaTiming::DisplaySync),
            (4, DmaTiming::MainMemDisplayFifo),
            (5, DmaTiming::Slot1),
            (6, DmaTiming::Slot2),
            (7, DmaTiming::GxFifo),
        ] {
            let mut d = d.clone();
            d.channels[0].control = bits << 27;
            assert_eq!(d.timing(0), expected);
        }
    }

    #[test]
    fn test_arm7_timing_modes() {
        let d = DmaController::new(false);
        for (bits, expected) in [
            (0, DmaTiming::Immediate),
            (1, DmaTiming::VBlank),
            (2, DmaTiming::Slot1),
            (3, DmaTiming::Special),
        ] {
            let mut d = d.clone();
            d.channels[0].control = bits << 27;
            assert_eq!(d.timing(0), expected);
        }
    }

    #[test]
    fn test_one_shot_clears_enable() {
        let mut d = DmaController::new(true);
        d.write_control(0, 1 << 31);
        d.finish_transfer(0);
        assert_eq!(d.channels[0].control & (1 << 31), 0);
        assert!(!d.channels[0].active);
    }

    #[test]
    fn test_repeat_keeps_active_and_reloads() {
        let mut d = DmaController::new(true);
        d.write_count(0, 4);
        d.write_control(0, (1 << 31) | (1 << 25)); // enable + repeat
        // Simulate count drained
        d.channels[0].internal_count = 0;
        d.finish_transfer(0);
        assert!(d.channels[0].active);
        assert_eq!(d.channels[0].internal_count, 4);
    }
}

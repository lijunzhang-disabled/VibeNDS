//! NDS audio — 16 hardware mixing channels on the ARM7 side.
//!
//! Each channel reads samples from a source address (Main RAM or VRAM),
//! advances a per-channel timer at the configured frequency, decodes the
//! current sample, applies channel volume + pan, and feeds into the
//! 16-channel mixer. The mixer outputs stereo i16 at 32768 Hz.
//!
//! Register layout (per GBATEK §"DS Sound"):
//!
//! ```text
//! 0x04000400 + ch * 0x10 + 0x00  SOUNDxCNT  (32-bit)
//!   [6:0]    Volume mul (0..127)
//!   [9:8]    Volume divider (0=/1, 1=/2, 2=/4, 3=/16)
//!   [22:16]  Pan (0..127, 64=center)
//!   [26:24]  Wave duty (PSG channels 8-13)
//!   [28:27]  Repeat mode (0=manual, 1=loop, 2=one-shot)
//!   [30:29]  Format (0=PCM8, 1=PCM16, 2=IMA-ADPCM, 3=PSG/Noise)
//!   [31]     Start/Status bit
//!
//! + 0x04  SOUNDxSAD (32-bit)  source byte address
//! + 0x08  SOUNDxTMR (16-bit)  timer reload = 0x10000 - period
//! + 0x0A  SOUNDxPNT (16-bit)  loop point (in 4-byte units, from start)
//! + 0x0C  SOUNDxLEN (32-bit)  total length after loop point (in 4-byte units)
//!
//! 0x04000500  SOUNDCNT (16-bit)
//!   [6:0]    Master volume
//!   [9:8]    Left output source (0=mixer, 1=ch1, 2=ch3, 3=ch1+3)
//!   [11:10]  Right output source (same)
//!   [12]     Output ch1 to mixer (0=mix, 1=skip)
//!   [13]     Output ch3 to mixer
//!   [15]     Master enable
//!
//! 0x04000504  SOUNDBIAS (16-bit) — output bias, default 0x200
//! ```

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub mod sample;
pub mod mixer;

pub use sample::SampleFormat;

/// Output sample rate. Real hardware nominally outputs at 32768 Hz.
pub const OUTPUT_HZ: u32 = 32768;

/// ARM7 clock divided by 32768 = how many ARM7 cycles per output sample.
pub const ARM7_CYCLES_PER_SAMPLE: u32 = crate::ARM7_CLOCK_HZ / OUTPUT_HZ;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepeatMode {
    Manual,
    Loop,
    OneShot,
}

impl RepeatMode {
    pub fn from_bits(b: u32) -> Self {
        match b & 0x3 {
            0 => RepeatMode::Manual,
            1 => RepeatMode::Loop,
            _ => RepeatMode::OneShot,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    /// Programmed `SOUNDxCNT` value. Bit 31 is the start/enable bit.
    pub cnt: u32,
    pub sad: u32,
    pub tmr: u16,
    pub pnt: u16,
    pub len: u32,

    /// Channel is currently playing (bit 31 of CNT was set and the channel
    /// hasn't exhausted its samples).
    pub active: bool,

    /// Internal sample-position counter (in 4-byte units, like SAD).
    pub pos_word: u32,
    /// Sub-position fractional (for non-integer timer ratios). 16-bit precision.
    pub pos_frac: u32,

    /// Last decoded mono sample, signed 16-bit. We re-emit this each
    /// output tick until the channel's per-sample timer advances.
    pub current_sample: i16,

    /// IMA-ADPCM decoder state (predictor + step index). Only used when
    /// format == ADPCM.
    pub adpcm_predictor: i16,
    pub adpcm_step_index: u8,
    /// Saved snapshot at the loop point (PNT) for ADPCM. Restored when
    /// the channel loops so the predictor is correct at loop start.
    pub adpcm_loop_predictor: i16,
    pub adpcm_loop_step_index: u8,

    /// LFSR for noise channels.
    pub noise_lfsr: u16,

    /// Wave-duty position for PSG channels.
    pub psg_phase: u32,
}

impl Channel {
    pub fn new() -> Self {
        Channel {
            cnt: 0, sad: 0, tmr: 0, pnt: 0, len: 0,
            active: false,
            pos_word: 0,
            pos_frac: 0,
            current_sample: 0,
            adpcm_predictor: 0,
            adpcm_step_index: 0,
            adpcm_loop_predictor: 0,
            adpcm_loop_step_index: 0,
            noise_lfsr: 0x7FFF,
            psg_phase: 0,
        }
    }

    #[inline] pub fn volume_mul(&self) -> u32 { self.cnt & 0x7F }
    #[inline] pub fn volume_div(&self) -> u32 {
        match (self.cnt >> 8) & 0x3 {
            0 => 0, 1 => 1, 2 => 2, _ => 4,
        }
    }
    #[inline] pub fn pan(&self) -> u32 { (self.cnt >> 16) & 0x7F }
    #[inline] pub fn duty(&self) -> u32 { (self.cnt >> 24) & 0x7 }
    #[inline] pub fn repeat(&self) -> RepeatMode { RepeatMode::from_bits(self.cnt >> 27) }
    #[inline] pub fn format(&self) -> SampleFormat { SampleFormat::from_bits(self.cnt >> 29) }

    /// Called on a 0→1 transition of the start bit (CNT bit 31).
    pub fn restart(&mut self, channel_id: usize) {
        self.active = true;
        self.pos_word = 0;
        self.pos_frac = 0;
        self.current_sample = 0;
        self.psg_phase = 0;
        // Noise channels (14, 15) reset their LFSR to the standard seed.
        if channel_id >= 14 {
            self.noise_lfsr = 0x7FFF;
        }
    }

    /// Per-output-sample period (in ARM7 cycles). TMR stores the reload
    /// value as `0x10000 - period`, so period = `0x10000 - TMR`.
    pub fn period_cycles(&self) -> u32 {
        let p = (0x10000 - self.tmr as u32).max(1);
        // The channel timer ticks at ARM7_CLOCK_HZ / 2 (per GBATEK); we
        // approximate with the ARM7 clock directly for simplicity.
        p
    }
}

impl Default for Channel {
    fn default() -> Self { Self::new() }
}

/// All 16 channels + global registers + output queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audio {
    pub channels: [Channel; 16],

    /// `SOUNDCNT` (16-bit).
    pub master_cnt: u16,
    /// `SOUNDBIAS` (low 10 bits).
    pub bias: u16,

    /// Accumulator for cycles since the last mixer tick. When this
    /// crosses ARM7_CYCLES_PER_SAMPLE, we push one stereo sample.
    pub cycle_accumulator: u32,

    /// Output ring (interleaved L, R, L, R, ...). Frontend pulls via
    /// `Nds::drain_audio`. Bounded; old samples are dropped on overflow.
    pub output: VecDeque<i16>,
}

/// Maximum queued samples (1 second at 32768 Hz stereo = 65 536). The
/// frontend typically pulls every frame so we never actually fill this;
/// the bound is a safety net.
const OUTPUT_CAP: usize = 65_536;

impl Audio {
    pub fn new() -> Self {
        Audio {
            channels: std::array::from_fn(|_| Channel::new()),
            master_cnt: 0,
            bias: 0x200,
            cycle_accumulator: 0,
            output: VecDeque::with_capacity(8192),
        }
    }

    #[inline] pub fn master_enabled(&self) -> bool { self.master_cnt & (1 << 15) != 0 }
    #[inline] pub fn master_volume(&self) -> u32 { (self.master_cnt & 0x7F) as u32 }

    /// CPU-side read of `SOUNDxCNT` (returns the programmed value with
    /// the start bit reflecting current active state).
    pub fn read_cnt(&self, ch: usize) -> u32 {
        let mut v = self.channels[ch].cnt;
        if self.channels[ch].active { v |= 1 << 31; } else { v &= !(1 << 31); }
        v
    }

    /// CPU-side write of `SOUNDxCNT`. Setting bit 31 (0→1 transition)
    /// restarts the channel.
    pub fn write_cnt(&mut self, ch: usize, val: u32) {
        let prev_start = self.channels[ch].cnt & (1 << 31) != 0;
        self.channels[ch].cnt = val;
        let new_start = val & (1 << 31) != 0;
        if !prev_start && new_start {
            self.channels[ch].restart(ch);
        } else if !new_start {
            self.channels[ch].active = false;
        }
    }

    /// Pop interleaved stereo samples into `out`. Returns count written.
    /// Underflow returns silence (0).
    pub fn drain(&mut self, out: &mut [i16]) -> usize {
        let mut n = 0;
        while n < out.len() {
            if let Some(s) = self.output.pop_front() {
                out[n] = s;
                n += 1;
            } else {
                break;
            }
        }
        // Pad with silence if requested more than available.
        for sample in &mut out[n..] { *sample = 0; }
        n
    }

    /// Push one stereo pair onto the output ring. Drops oldest on overflow.
    pub(crate) fn push_stereo(&mut self, l: i16, r: i16) {
        if self.output.len() + 2 > OUTPUT_CAP {
            // Drop a stereo pair from the front (preserve interleaving).
            self.output.pop_front();
            self.output.pop_front();
        }
        self.output.push_back(l);
        self.output.push_back(r);
    }
}

impl Default for Audio {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_start_bit_starts_channel() {
        let mut a = Audio::new();
        a.write_cnt(0, 1 << 31);
        assert!(a.channels[0].active);
    }

    #[test]
    fn test_clearing_start_bit_stops_channel() {
        let mut a = Audio::new();
        a.write_cnt(0, 1 << 31);
        assert!(a.channels[0].active);
        a.write_cnt(0, 0);
        assert!(!a.channels[0].active);
    }

    #[test]
    fn test_read_cnt_reflects_active_state() {
        let mut a = Audio::new();
        a.write_cnt(0, 1 << 31);
        assert_eq!(a.read_cnt(0) >> 31, 1);
        a.channels[0].active = false;
        assert_eq!(a.read_cnt(0) >> 31, 0);
    }

    #[test]
    fn test_drain_pads_with_silence_on_underflow() {
        let mut a = Audio::new();
        a.push_stereo(100, 200);
        let mut buf = [0i16; 6];
        let n = a.drain(&mut buf);
        assert_eq!(n, 2);
        assert_eq!(buf[0], 100);
        assert_eq!(buf[1], 200);
        assert!(buf[2..].iter().all(|&s| s == 0));
    }

    #[test]
    fn test_format_decoding() {
        let mut a = Audio::new();
        a.write_cnt(0, 0 << 29); // PCM8
        assert!(matches!(a.channels[0].format(), SampleFormat::Pcm8));
        a.write_cnt(1, 1 << 29);
        assert!(matches!(a.channels[1].format(), SampleFormat::Pcm16));
        a.write_cnt(2, 2 << 29);
        assert!(matches!(a.channels[2].format(), SampleFormat::Adpcm));
        a.write_cnt(3, 3 << 29);
        assert!(matches!(a.channels[3].format(), SampleFormat::PsgOrNoise));
    }
}

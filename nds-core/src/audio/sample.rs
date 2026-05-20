//! Per-channel sample fetch + format-specific decoders.
//!
//! Five formats:
//!
//! - **PCM8**: one signed byte per sample, source advances 1 byte per tick.
//! - **PCM16**: one signed 16-bit halfword per sample.
//! - **IMA-ADPCM**: 4-bit ADPCM with a 4-byte block header (16-bit signed
//!   initial predictor + 16-bit initial step index). Each nibble is one
//!   sample. The predictor + step index advance per-sample via the
//!   standard IMA tables.
//! - **PSG**: square wave (channels 8-13). Wave duty selects high vs low
//!   ratio. Period from the channel timer.
//! - **Noise**: 15-bit LFSR (channels 14-15). Period from the channel timer.

use serde::{Deserialize, Serialize};

use super::{Channel, RepeatMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SampleFormat {
    Pcm8,
    Pcm16,
    Adpcm,
    PsgOrNoise,
}

impl SampleFormat {
    pub fn from_bits(b: u32) -> Self {
        match b & 0x3 {
            0 => SampleFormat::Pcm8,
            1 => SampleFormat::Pcm16,
            2 => SampleFormat::Adpcm,
            _ => SampleFormat::PsgOrNoise,
        }
    }
}

/// IMA-ADPCM step table (89 entries).
pub const IMA_STEP_TABLE: [i32; 89] = [
       7,    8,    9,   10,   11,   12,   13,   14,   16,   17,
      19,   21,   23,   25,   28,   31,   34,   37,   41,   45,
      50,   55,   60,   66,   73,   80,   88,   97,  107,  118,
     130,  143,  157,  173,  190,  209,  230,  253,  279,  307,
     337,  371,  408,  449,  494,  544,  598,  658,  724,  796,
     876,  963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358,
    5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899,
    15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// IMA-ADPCM step-index delta table for the 4-bit nibble's low 3 bits.
pub const IMA_INDEX_TABLE: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// Decode one IMA-ADPCM nibble into a delta + new predictor.
pub fn decode_adpcm_nibble(predictor: i16, step_index: u8, nibble: u8) -> (i16, u8) {
    let step = IMA_STEP_TABLE[step_index as usize];
    // Diff = step / 8 + step / 4 * (n0?1:0) + step / 2 * (n1?1:0) + step * (n2?1:0)
    let mut diff = step >> 3;
    if nibble & 1 != 0 { diff += step >> 2; }
    if nibble & 2 != 0 { diff += step >> 1; }
    if nibble & 4 != 0 { diff += step; }
    let sign = nibble & 8 != 0;
    let new_pred = if sign {
        (predictor as i32 - diff).max(i16::MIN as i32)
    } else {
        (predictor as i32 + diff).min(i16::MAX as i32)
    };
    let mut new_idx = (step_index as i32) + IMA_INDEX_TABLE[(nibble & 7) as usize];
    new_idx = new_idx.clamp(0, 88);
    (new_pred as i16, new_idx as u8)
}

/// PSG wave-duty pattern: bit positions where the square wave is "high".
/// `duty` is 0..7 — number of high samples in an 8-sample period (minus 1).
/// E.g. duty=0 → 1/8 high (12.5%), duty=7 → 8/8 high (100% — silent on real HW).
#[inline]
pub fn psg_square_sample(phase: u32, duty: u32) -> i16 {
    let high_count = (duty + 1).min(7);
    if (phase & 7) < high_count { i16::MAX / 2 } else { -i16::MAX / 2 }
}

/// 15-bit LFSR step. Returns the next LFSR and the sample value (+/-).
pub fn noise_step(lfsr: u16) -> (u16, i16) {
    // Standard LFSR for noise: bit 0 XOR bit 1, fed into bit 14.
    let bit = (lfsr ^ (lfsr >> 1)) & 1;
    let next = (lfsr >> 1) | (bit << 14);
    let sample = if next & 1 != 0 { i16::MAX / 4 } else { -i16::MAX / 4 };
    (next & 0x7FFF, sample)
}

/// Read N bytes from the source address `sad + offset` via a `bus_read8`
/// closure. The closure is the only thing the audio module needs from the
/// rest of the system, so we keep it minimal.
pub fn fetch_byte(sad: u32, offset: u32, bus_read8: &mut dyn FnMut(u32) -> u8) -> u8 {
    bus_read8(sad.wrapping_add(offset))
}

/// Advance one channel by one output-sample period, fetching/decoding the
/// next sample as needed. `bus_read8` reads main RAM / VRAM via the ARM7
/// bus. Returns the channel's new sample as signed i16.
///
/// The decoder handles loop / one-shot per `RepeatMode`. End-of-buffer for
/// one-shot turns the channel off; for loop, position resets to PNT.
pub fn advance_channel(
    ch: &mut Channel,
    channel_id: usize,
    bus_read8: &mut dyn FnMut(u32) -> u8,
) -> i16 {
    if !ch.active { return 0; }

    let fmt = ch.format();
    let pnt = ch.pnt as u32;
    let len = ch.len;
    let end_word = pnt + len;

    let sample = match fmt {
        SampleFormat::Pcm8 => {
            let byte = bus_read8(ch.sad.wrapping_add(ch.pos_word * 4 + ch.pos_frac));
            ch.pos_frac += 1;
            if ch.pos_frac >= 4 { ch.pos_frac = 0; ch.pos_word += 1; }
            ((byte as i8) as i16).saturating_mul(256)
        }
        SampleFormat::Pcm16 => {
            let base = ch.sad.wrapping_add(ch.pos_word * 4 + ch.pos_frac);
            let lo = bus_read8(base) as u16;
            let hi = bus_read8(base.wrapping_add(1)) as u16;
            ch.pos_frac += 2;
            if ch.pos_frac >= 4 { ch.pos_frac = 0; ch.pos_word += 1; }
            (lo | (hi << 8)) as i16
        }
        SampleFormat::Adpcm => {
            // First 4-byte word at sad is the header.
            if ch.pos_word == 0 && ch.pos_frac == 0 {
                let lo = bus_read8(ch.sad) as u16;
                let hi = bus_read8(ch.sad.wrapping_add(1)) as u16;
                ch.adpcm_predictor = (lo | (hi << 8)) as i16;
                let idx_lo = bus_read8(ch.sad.wrapping_add(2)) as u16;
                let idx_hi = bus_read8(ch.sad.wrapping_add(3)) as u16;
                ch.adpcm_step_index = ((idx_lo | (idx_hi << 8)) & 0x7F) as u8;
                ch.pos_word = 1;
                ch.pos_frac = 0;
                return ch.adpcm_predictor;
            }
            // Snapshot ADPCM state at loop point (so loop restores it).
            if ch.pos_word == pnt && ch.pos_frac == 0 {
                ch.adpcm_loop_predictor = ch.adpcm_predictor;
                ch.adpcm_loop_step_index = ch.adpcm_step_index;
            }
            let byte = bus_read8(ch.sad.wrapping_add(ch.pos_word * 4 + (ch.pos_frac >> 1)));
            let nibble = if ch.pos_frac & 1 == 0 { byte & 0xF } else { byte >> 4 };
            let (new_pred, new_idx) = decode_adpcm_nibble(ch.adpcm_predictor, ch.adpcm_step_index, nibble);
            ch.adpcm_predictor = new_pred;
            ch.adpcm_step_index = new_idx;
            ch.pos_frac += 1;
            if ch.pos_frac >= 8 { ch.pos_frac = 0; ch.pos_word += 1; }
            new_pred
        }
        SampleFormat::PsgOrNoise => {
            if channel_id >= 14 {
                let (new_lfsr, sample) = noise_step(ch.noise_lfsr);
                ch.noise_lfsr = new_lfsr;
                sample
            } else if channel_id >= 8 {
                let s = psg_square_sample(ch.psg_phase, ch.duty());
                ch.psg_phase = ch.psg_phase.wrapping_add(1);
                s
            } else {
                // Channels 0..7 use PCM/ADPCM only; format=3 is undefined.
                0
            }
        }
    };

    // Handle end-of-sample (PCM / ADPCM only; PSG/Noise never end).
    if matches!(fmt, SampleFormat::Pcm8 | SampleFormat::Pcm16 | SampleFormat::Adpcm)
        && ch.pos_word >= end_word
    {
        match ch.repeat() {
            RepeatMode::OneShot | RepeatMode::Manual => {
                ch.active = false;
            }
            RepeatMode::Loop => {
                ch.pos_word = pnt;
                ch.pos_frac = 0;
                // For ADPCM, restore the predictor/step at the loop point.
                if matches!(fmt, SampleFormat::Adpcm) {
                    ch.adpcm_predictor = ch.adpcm_loop_predictor;
                    ch.adpcm_step_index = ch.adpcm_loop_step_index;
                }
            }
        }
    }

    ch.current_sample = sample;
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adpcm_round_trip_predictor_advances() {
        // Encode a known signal would be ideal but we can just verify the
        // decoder advances state predictably.
        let (p1, i1) = decode_adpcm_nibble(0, 0, 0x4); // high bit: positive step
        assert!(p1 > 0, "positive nibble should increase predictor; got {}", p1);
        assert!(i1 > 0, "step index should advance; got {}", i1);

        let (p2, _i2) = decode_adpcm_nibble(p1, i1, 0xC); // sign bit + step
        assert!(p2 < p1, "negative nibble should decrease predictor; got {} (prev {})", p2, p1);
    }

    #[test]
    fn test_psg_square_duty() {
        // duty=3 → 4 samples high, 4 low per 8.
        assert_eq!(psg_square_sample(0, 3) > 0, true);
        assert_eq!(psg_square_sample(3, 3) > 0, true);
        assert_eq!(psg_square_sample(4, 3) < 0, true);
        assert_eq!(psg_square_sample(7, 3) < 0, true);
    }

    #[test]
    fn test_noise_lfsr_advances() {
        let initial = 0x7FFFu16;
        let (next, _s) = noise_step(initial);
        assert_ne!(next, initial, "LFSR should change state each step");
    }

    #[test]
    fn test_pcm8_advance_reads_source() {
        let mut ch = Channel::new();
        ch.cnt = (1 << 31); // start, format=PCM8
        ch.sad = 0x1000;
        ch.len = 4;
        ch.active = true;
        let mut samples = vec![0u8; 0x20];
        samples[0] = 0x10;
        samples[1] = 0x20;
        let mut read = |addr: u32| -> u8 {
            let off = (addr - 0x1000) as usize;
            samples.get(off).copied().unwrap_or(0)
        };
        let s0 = advance_channel(&mut ch, 0, &mut read);
        // PCM8 amplifies 8-bit signed to 16-bit; 0x10 = 16 → 16 * 256 = 4096.
        assert_eq!(s0, 4096);
        let s1 = advance_channel(&mut ch, 0, &mut read);
        assert_eq!(s1, 0x20 * 256);
    }

    #[test]
    fn test_pcm8_loops_at_end() {
        let mut ch = Channel::new();
        ch.cnt = (1 << 31) | (1 << 27); // start + loop
        ch.sad = 0x1000;
        ch.pnt = 0;
        ch.len = 1; // one 4-byte word — 4 samples then loop
        ch.active = true;
        let samples = vec![1u8, 2, 3, 4, 0, 0, 0, 0];
        let mut read = |addr: u32| -> u8 {
            let off = (addr - 0x1000) as usize;
            samples.get(off).copied().unwrap_or(0xFF)
        };
        // Run 6 advances; should see 1,2,3,4 then loop to 1,2.
        let mut got = vec![];
        for _ in 0..6 {
            got.push(advance_channel(&mut ch, 0, &mut read));
        }
        // After 4 samples (pos_word=1), end_word=0+1=1 met; loops back to pnt=0.
        // So bytes 1,2,3,4,1,2.
        assert_eq!(got[0], 1 * 256);
        assert_eq!(got[3], 4 * 256);
        assert_eq!(got[4], 1 * 256, "should have looped");
        assert!(ch.active);
    }

    #[test]
    fn test_pcm8_one_shot_deactivates() {
        let mut ch = Channel::new();
        ch.cnt = (1 << 31) | (2 << 27); // start + one-shot
        ch.sad = 0;
        ch.pnt = 0;
        ch.len = 1; // 4 samples then stop
        ch.active = true;
        let mut read = |_addr: u32| -> u8 { 0x10 };
        for _ in 0..4 {
            let _ = advance_channel(&mut ch, 0, &mut read);
        }
        // One more advance should hit end + deactivate.
        let _ = advance_channel(&mut ch, 0, &mut read);
        assert!(!ch.active);
    }
}

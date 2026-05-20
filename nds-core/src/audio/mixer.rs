//! 16-channel audio mixer.
//!
//! Runs once per output sample (every `ARM7_CYCLES_PER_SAMPLE` ARM7
//! cycles). For each active channel: fetch its current sample, apply
//! channel volume + pan, accumulate into the stereo bus. Then apply
//! master volume + SOUNDBIAS and push to the output ring.

use super::{sample::advance_channel, Audio, ARM7_CYCLES_PER_SAMPLE};

/// Advance the audio mixer by `cycles` ARM7 cycles. May push 0 or more
/// stereo samples into the output ring depending on how many sample-
/// period boundaries we crossed.
///
/// `bus_read8` is the ARM7-side byte read callback the channels use to
/// fetch sample data from main RAM / VRAM.
pub fn tick(audio: &mut Audio, cycles: u32, bus_read8: &mut dyn FnMut(u32) -> u8) {
    audio.cycle_accumulator += cycles;
    while audio.cycle_accumulator >= ARM7_CYCLES_PER_SAMPLE {
        audio.cycle_accumulator -= ARM7_CYCLES_PER_SAMPLE;
        produce_sample(audio, bus_read8);
    }
}

fn produce_sample(audio: &mut Audio, bus_read8: &mut dyn FnMut(u32) -> u8) {
    if !audio.master_enabled() {
        audio.push_stereo(0, 0);
        return;
    }

    let mut acc_l: i64 = 0;
    let mut acc_r: i64 = 0;

    for ch_id in 0..16 {
        let ch = &mut audio.channels[ch_id];
        if !ch.active { continue; }

        let sample = advance_channel(ch, ch_id, bus_read8);

        // Channel-side gain: vol_mul / 128 × 1 / (2^vol_div).
        let vol_mul = ch.volume_mul() as i64;
        let vol_div = ch.volume_div();
        let gained = (sample as i64 * vol_mul) >> (7 + vol_div); // / 128 / 2^div

        // Pan: 0 = full left, 64 = center, 127 = full right.
        // Per GBATEK: left  = sample * (127 - pan) / 128
        //             right = sample * pan / 128
        let pan = ch.pan() as i64;
        let l = (gained * (127 - pan)) >> 7;
        let r = (gained * pan) >> 7;

        acc_l += l;
        acc_r += r;
    }

    // Master volume + SOUNDBIAS. Master is 0..127; bias is a DC offset
    // added before the DAC clamps to 10-bit unsigned. We model master as
    // a simple gain (0..1) and skip the actual DAC clamping behavior;
    // most games stay well within range.
    let master = audio.master_volume() as i64;
    let l_final = (acc_l * master) >> 7;
    let r_final = (acc_r * master) >> 7;

    let l_clamped = l_final.clamp(i16::MIN as i64, i16::MAX as i64) as i16;
    let r_clamped = r_final.clamp(i16::MIN as i64, i16::MAX as i64) as i16;
    audio.push_stereo(l_clamped, r_clamped);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Channel;

    #[test]
    fn test_master_disabled_outputs_silence() {
        let mut a = Audio::new();
        // Channel 0 with non-zero state but master enable bit 15 NOT set.
        a.channels[0].cnt = (1 << 31) | 0x7F; // start + full volume
        a.channels[0].active = true;
        a.master_cnt = 0; // bit 15 clear
        let mut read = |_: u32| -> u8 { 0x40 };
        tick(&mut a, ARM7_CYCLES_PER_SAMPLE, &mut read);
        let mut out = [0i16; 2];
        a.drain(&mut out);
        assert_eq!(out, [0, 0]);
    }

    #[test]
    fn test_one_channel_produces_sample() {
        let mut a = Audio::new();
        a.master_cnt = (1 << 15) | 127; // master enable + full master volume
        a.channels[0].cnt = (1 << 31) | 127 | (64 << 16); // start + full ch vol + center pan
        a.channels[0].active = true;
        a.channels[0].sad = 0x100;
        a.channels[0].len = 4;
        let mut read = |_: u32| -> u8 { 0x40 };
        tick(&mut a, ARM7_CYCLES_PER_SAMPLE, &mut read);
        let mut out = [0i16; 2];
        a.drain(&mut out);
        // Should produce *some* non-zero sample on both L and R (center pan).
        assert!(out[0] != 0);
        assert!(out[1] != 0);
        // Center pan splits roughly evenly. Pan=64 is "almost center" —
        // the formula is L=src×(127-pan)/128, R=src×pan/128, so center
        // has a ~1/128 asymmetry. Allow that.
        let ratio = (out[0] as f32) / (out[1] as f32);
        assert!((0.9..1.1).contains(&ratio), "center pan: L/R = {} (L={} R={})", ratio, out[0], out[1]);
    }

    #[test]
    fn test_pan_full_left_silences_right() {
        let mut a = Audio::new();
        a.master_cnt = (1 << 15) | 127;
        a.channels[0].cnt = (1 << 31) | 127 | (0 << 16); // pan = 0 (full left)
        a.channels[0].active = true;
        a.channels[0].len = 4;
        let mut read = |_: u32| -> u8 { 0x40 };
        tick(&mut a, ARM7_CYCLES_PER_SAMPLE, &mut read);
        let mut out = [0i16; 2];
        a.drain(&mut out);
        assert!(out[0] != 0);
        // Right side should be 0 (within rounding).
        assert!(out[1].abs() < 50, "expected right channel silent, got {}", out[1]);
    }

    #[test]
    fn test_cycle_accumulator_produces_one_sample_per_period() {
        let mut a = Audio::new();
        a.master_cnt = 1 << 15;
        let mut read = |_: u32| -> u8 { 0 };
        // Half a sample period: no output yet.
        tick(&mut a, ARM7_CYCLES_PER_SAMPLE / 2, &mut read);
        assert_eq!(a.output.len(), 0);
        // Other half: one sample.
        tick(&mut a, ARM7_CYCLES_PER_SAMPLE / 2 + 1, &mut read);
        assert_eq!(a.output.len(), 2);
    }
}

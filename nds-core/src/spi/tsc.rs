//! Touchscreen Controller (ADS7843) over SPI device 2.
//!
//! Protocol: each conversion is **3 bytes** on the SPI bus.
//!
//! - **Byte 0**: control word.
//!   ```text
//!   [7]   start (always 1 for a valid request)
//!   [6:4] channel select:
//!         001 = Y position
//!         010 = battery voltage
//!         011 = Z1 (touch pressure)
//!         100 = Z2 (touch pressure)
//!         101 = X position
//!         110 = AUX  (auxiliary input)
//!         111 = temperature
//!   [3]   12-bit (0) / 8-bit (1) result
//!   [2]   single-ended (0) / differential (1) reference
//!   [1:0] power-down mode
//!   ```
//! - **Byte 1**: high 7 bits of the 12-bit ADC result (top bit padded 0).
//! - **Byte 2**: low 5 bits of the result (shifted left by 3, rest 0).
//!
//! The CPU typically sends a control byte, reads byte-out, sends a dummy
//! 0, reads the next byte-out, sends another dummy 0, reads the final
//! byte-out. Three SPI transfers per ADC sample.
//!
//! Coordinate mapping: SDL2 gives us screen-pixel coords (0..256, 0..192).
//! Real ADS7843 returns raw ADC values that the game converts to screen
//! coords using a calibration matrix stored in firmware. We use the
//! firmware's *default* calibration (set up by `spi::firmware`) so the
//! game's conversion produces the screen pixels we originally fed in.

use serde::{Deserialize, Serialize};

/// Default ADC calibration constants — chosen to match the synthesized
/// firmware calibration block so screen pixels round-trip cleanly.
/// (Real DS firmware uses ADC ranges similar to these.)
pub const ADC_X1: u16 = 0x0200; // raw ADC for screen X = 0
pub const ADC_X2: u16 = 0x0E00; // raw ADC for screen X = 255
pub const ADC_Y1: u16 = 0x0200;
pub const ADC_Y2: u16 = 0x0E00;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    /// Waiting for control byte.
    Idle,
    /// Control byte consumed; latched the 12-bit value; sending high byte next.
    HighByte { value12: u16, channel: u8, eight_bit: bool },
    /// Sent high byte; sending low byte next.
    LowByte { value12: u16, eight_bit: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tsc {
    /// SDL-side screen coords (0..256, 0..192). Set by the frontend.
    pub screen_x: u16,
    pub screen_y: u16,
    pub pen_down: bool,

    phase: Phase,
}

impl Tsc {
    pub fn new() -> Self {
        Tsc {
            screen_x: 0,
            screen_y: 0,
            pen_down: false,
            phase: Phase::Idle,
        }
    }

    pub fn reset(&mut self) {
        self.phase = Phase::Idle;
    }

    /// Update from the frontend each frame.
    pub fn set_touch(&mut self, x: u16, y: u16, down: bool) {
        self.screen_x = x.min(255);
        self.screen_y = y.min(191);
        self.pen_down = down;
    }

    /// Map screen X (0..255) → ADC value using the [ADC_X1, ADC_X2] range.
    fn adc_x(&self) -> u16 {
        let span = ADC_X2 - ADC_X1;
        ADC_X1 + ((self.screen_x as u32 * span as u32) / 255) as u16
    }

    fn adc_y(&self) -> u16 {
        let span = ADC_Y2 - ADC_Y1;
        ADC_Y1 + ((self.screen_y as u32 * span as u32) / 191) as u16
    }

    /// Z (pressure): non-zero when pen is down, zero when not.
    fn adc_z(&self) -> u16 {
        if self.pen_down { 0x0800 } else { 0x0000 }
    }

    pub fn xfer(&mut self, byte_in: u8, _hold: bool) -> u8 {
        match self.phase {
            Phase::Idle => {
                if byte_in & 0x80 == 0 {
                    // Not a start bit — return 0 and stay idle.
                    return 0;
                }
                let channel = (byte_in >> 4) & 0x7;
                let eight_bit = byte_in & 0x08 != 0;

                let raw = match channel {
                    1 => self.adc_y(),
                    2 => 0x0CCC,           // battery voltage (~80% of full scale)
                    3 | 4 => self.adc_z(), // touch pressure
                    5 => self.adc_x(),
                    _ => 0,                // AUX / temperature — return 0
                };

                // Real chip returns 0 for the channel-byte response (the
                // result lands on the *next* two transfers).
                self.phase = Phase::HighByte { value12: raw & 0x0FFF, channel, eight_bit };
                0
            }
            Phase::HighByte { value12, channel: _, eight_bit } => {
                let hi = if eight_bit {
                    // 8-bit mode: full result in one byte; second byte is 0.
                    ((value12 >> 4) & 0xFF) as u8
                } else {
                    // 12-bit mode: high 7 bits with top bit padded 0.
                    ((value12 >> 5) & 0x7F) as u8
                };
                self.phase = Phase::LowByte { value12, eight_bit };
                hi
            }
            Phase::LowByte { value12, eight_bit } => {
                let lo = if eight_bit {
                    0
                } else {
                    ((value12 << 3) & 0xF8) as u8
                };
                self.phase = Phase::Idle;
                lo
            }
        }
    }
}

impl Default for Tsc {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue a 3-byte conversion sequence for the given channel and return
    /// the reconstructed 12-bit value.
    fn read_channel(tsc: &mut Tsc, channel: u8) -> u16 {
        let control = 0x80 | (channel << 4); // start bit + channel
        let _ = tsc.xfer(control, true);
        let hi = tsc.xfer(0, true);
        let lo = tsc.xfer(0, false);
        // 12-bit reconstruction: hi has bits [11:5], lo has bits [4:0] in [7:3].
        ((hi as u16) << 5) | ((lo as u16) >> 3)
    }

    #[test]
    fn test_x_endpoints_round_trip() {
        let mut tsc = Tsc::new();
        tsc.set_touch(0, 0, true);
        assert_eq!(read_channel(&mut tsc, 5), ADC_X1, "X=0 should map to ADC_X1");
        tsc.set_touch(255, 0, true);
        assert_eq!(read_channel(&mut tsc, 5), ADC_X2, "X=255 should map to ADC_X2");
    }

    #[test]
    fn test_y_endpoints_round_trip() {
        let mut tsc = Tsc::new();
        tsc.set_touch(0, 0, true);
        assert_eq!(read_channel(&mut tsc, 1), ADC_Y1);
        tsc.set_touch(0, 191, true);
        assert_eq!(read_channel(&mut tsc, 1), ADC_Y2);
    }

    #[test]
    fn test_z_nonzero_when_pen_down() {
        let mut tsc = Tsc::new();
        tsc.set_touch(100, 100, true);
        assert!(read_channel(&mut tsc, 3) > 0);
        tsc.set_touch(100, 100, false);
        assert_eq!(read_channel(&mut tsc, 3), 0);
    }

    #[test]
    fn test_idle_byte_returns_zero() {
        let mut tsc = Tsc::new();
        // No start bit → return 0, stay idle.
        let r = tsc.xfer(0x00, false);
        assert_eq!(r, 0);
    }
}

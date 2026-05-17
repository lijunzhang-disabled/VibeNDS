//! Power Management IC over SPI device 0.
//!
//! Phase 5 keeps this minimal: the PMIC is a sink for boot-time writes
//! (backlight enable, sound enable, top/bottom LCD power) and returns
//! sane defaults on read. We store register writes for observability but
//! don't enforce them — the LCDs are always "powered on" in our model.
//!
//! Protocol: each SPI transaction is 2 bytes.
//!   byte 0: `[7]=write/read#, [6:0]=register index`
//!   byte 1: data (read returns this slot, write stores it)
//!
//! Registers (per GBATEK):
//!   0: control — bit 0 = sound enable, bit 4 = top backlight, bit 5 = bottom backlight, bit 6 = power-off-on-next-write
//!   1: battery / status
//!   2: amplifier gain (3 bits)
//!   3: amplifier enable
//!   4: low-battery indicator (RO; we return 0 = OK)

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    /// Waiting for first byte (register select).
    Idle,
    /// First byte consumed; reg index decoded; waiting for data byte.
    Address { reg: u8, write: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pmic {
    pub regs: [u8; 8],
    phase: Phase,
}

impl Pmic {
    pub fn new() -> Self {
        let mut regs = [0u8; 8];
        // Reasonable defaults — both backlights on, sound enabled.
        regs[0] = (1 << 0) | (1 << 4) | (1 << 5);
        Pmic { regs, phase: Phase::Idle }
    }

    pub fn reset(&mut self) {
        // CS deassert returns the device to idle. Per-register values
        // persist across transactions.
        self.phase = Phase::Idle;
    }

    pub fn xfer(&mut self, byte_in: u8, _hold: bool) -> u8 {
        match self.phase {
            Phase::Idle => {
                let write = (byte_in & 0x80) == 0;
                let reg = byte_in & 0x7F;
                self.phase = Phase::Address { reg, write };
                0 // first byte returns 0 (the device hasn't shifted anything yet)
            }
            Phase::Address { reg, write } => {
                let idx = (reg as usize) & 0x7;
                let response = self.regs[idx];
                if write {
                    self.regs[idx] = byte_in;
                }
                // Next byte (if hold) goes through the same address; we
                // don't advance the phase. Real hardware auto-increments
                // the register on extended transfers, but no game we care
                // about does multi-byte PMIC transactions, so this is
                // adequate.
                response
            }
        }
    }
}

impl Default for Pmic {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_default_control() {
        let mut p = Pmic::new();
        let _addr = p.xfer(0x80 | 0, false); // read reg 0
        let val = p.xfer(0x00, false);
        assert_eq!(val & 0x01, 0x01, "sound enable should be on by default");
        assert_eq!(val & 0x10, 0x10, "top backlight on");
        assert_eq!(val & 0x20, 0x20, "bottom backlight on");
    }

    #[test]
    fn test_write_then_read_back() {
        let mut p = Pmic::new();
        // Write 0x42 to reg 2 (amplifier gain). One SPI transaction.
        let _ = p.xfer(0x02, true);    // address byte, write
        let _ = p.xfer(0x42, false);   // data byte
        // Real hardware: hold=false → CS deassert → device resets.
        // Mirror that here so the next transaction starts from Idle.
        p.reset();

        let _ = p.xfer(0x80 | 0x02, true); // address byte, read
        let val = p.xfer(0x00, false);
        assert_eq!(val, 0x42);
    }
}

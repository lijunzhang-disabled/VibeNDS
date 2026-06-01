//! Main SPI bus (ARM7-only) at `0x040001C0..0x040001C3`.
//!
//! Three devices share the bus, selected by `SPICNT[8:9]`:
//!
//! - Device 0: **Power Management IC** (PMIC) — backlights, sound enable, power off.
//! - Device 1: **Firmware** (256 KB SPI flash) — user settings, WiFi cal data, optional boot menu.
//! - Device 2: **Touchscreen Controller** (TSC) — ADS7843-style 12-bit ADC.
//! - Device 3: reserved.
//!
//! Reference: GBATEK §"DS SPI Bus".
//!
//! ## Register layout
//!
//! ```text
//! 0x040001C0 SPICNT  (16-bit):
//!   [1:0]   baud rate (we ignore; software-visible only)
//!   [7]     busy (RO, 0 in our impl — transfers are instantaneous)
//!   [9:8]   device select  (0=PMIC, 1=Firmware, 2=TSC, 3=reserved)
//!   [10]    transfer size: 0 = 8-bit, 1 = 16-bit (we always shift bytes)
//!   [11]    chip select hold  ("more bytes coming, keep CS asserted")
//!   [14]    transfer-complete IRQ enable
//!   [15]    SPI bus enable (master)
//!
//! 0x040001C2 SPIDATA (16-bit, but only low 8 bits used):
//!   write: byte to shift out; triggers transfer; result lands here for next read
//!   read:  byte most recently shifted in from the selected device
//! ```

use serde::{Deserialize, Serialize};

pub mod firmware;
pub mod pmic;
pub mod tsc;

pub use firmware::Firmware;
pub use pmic::Pmic;
pub use tsc::Tsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Device {
    Pmic,
    Firmware,
    Tsc,
    Reserved,
}

impl Device {
    pub fn from_bits(bits: u16) -> Self {
        match (bits >> 8) & 0x3 {
            0 => Device::Pmic,
            1 => Device::Firmware,
            2 => Device::Tsc,
            _ => Device::Reserved,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpiBus {
    /// `SPICNT` register value (bit 7 — busy — is always read as 0).
    pub cnt: u16,
    /// `SPIDATA` value last read by the CPU (= byte the device returned
    /// on the most recent transfer).
    pub data: u8,
    /// True between successive bytes of a multi-byte transfer — set when
    /// CPU writes SPIDATA with `SPICNT.hold = 1`; cleared when CPU writes
    /// SPIDATA with hold = 0 (the final byte of the sequence).
    pub cs_held: bool,
    /// Last device asserted while `cs_held` is true. We track this
    /// separately because some devices reset their internal state machine
    /// when CS deasserts.
    pub current_device: Device,

    pub firmware: Firmware,
    pub tsc: Tsc,
    pub pmic: Pmic,
}

impl SpiBus {
    pub fn new() -> Self {
        SpiBus {
            cnt: 0,
            data: 0,
            cs_held: false,
            current_device: Device::Pmic,
            firmware: Firmware::new(),
            tsc: Tsc::new(),
            pmic: Pmic::new(),
        }
    }

    /// True if the master enable bit (bit 15) is set.
    pub fn enabled(&self) -> bool {
        self.cnt & (1 << 15) != 0
    }

    /// Returns the value the CPU reads from SPICNT (bit 7 always 0).
    pub fn read_cnt(&self) -> u16 {
        self.cnt & !(1 << 7)
    }

    pub fn write_cnt(&mut self, val: u16) {
        // Latch device-select even while disabled — homebrew sometimes
        // pre-configures CNT, then sets the enable bit on the same edge.
        self.cnt = val;
    }

    pub fn read_data(&self) -> u8 {
        self.data
    }

    /// Triggered by a CPU write to `SPIDATA`. Performs the byte exchange
    /// and returns `true` if `SPICNT.transfer_complete_irq_enable` is set
    /// (caller should raise `Irq::Spi` on the ARM7 controller).
    pub fn write_data(&mut self, byte_in: u8) -> bool {
        if !self.enabled() {
            // Bus disabled: write is dropped, but the data register
            // mirrors the written byte (per GBATEK).
            self.data = byte_in;
            return false;
        }

        let device = Device::from_bits(self.cnt);
        let hold = self.cnt & (1 << 11) != 0;

        // If CS just deasserted on the previous byte, reset whatever
        // device was last selected so its state machine starts fresh.
        if !self.cs_held && self.current_device != device {
            self.reset_device(self.current_device);
        }

        let byte_out = match device {
            Device::Pmic => self.pmic.xfer(byte_in, hold),
            Device::Firmware => self.firmware.xfer(byte_in, hold),
            Device::Tsc => self.tsc.xfer(byte_in, hold),
            Device::Reserved => 0xFF,
        };

        self.data = byte_out;
        self.current_device = device;

        // Track CS hold across this transfer for the *next* byte's reset
        // logic. The chip-select wire is conceptually asserted from the
        // first byte of a sequence (hold=1 on first..N-1, hold=0 on last).
        // When hold = 0 the device's state machine returns to "Idle" via
        // its xfer() return.
        self.cs_held = hold;
        if !hold {
            // Final byte of the sequence — reset the device's state.
            self.reset_device(device);
        }

        // Bit 14: transfer-complete IRQ enable.
        self.cnt & (1 << 14) != 0
    }

    fn reset_device(&mut self, dev: Device) {
        match dev {
            Device::Pmic => self.pmic.reset(),
            Device::Firmware => self.firmware.reset(),
            Device::Tsc => self.tsc.reset(),
            Device::Reserved => {}
        }
    }
}

impl Default for SpiBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_bus_does_not_xfer() {
        let mut spi = SpiBus::new();
        // No enable bit set
        let _ = spi.write_data(0xAA);
        assert_eq!(spi.read_data(), 0xAA, "disabled bus mirrors written byte");
    }

    #[test]
    fn test_device_selection_routes_correctly() {
        let mut spi = SpiBus::new();
        // Enable bus, select PMIC
        spi.write_cnt(1 << 15);
        spi.write_data(0x00);
        // PMIC default returns 0 — but we mainly care it didn't hit firmware
        // (which would error on a malformed command and have side effects).
        // Just check we don't panic.
        let _ = spi.read_data();
    }
}

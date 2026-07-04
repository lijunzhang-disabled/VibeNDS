//! Cart slot-1: header parsing and direct-boot loader.
//!
//! AUXSPI backup, KEY1/KEY2 encryption, and the slot-1 command protocol
//! arrive in later phases.

pub mod auxspi;
pub mod chip_id;
pub mod direct_boot;
pub mod header;

pub use auxspi::{AuxSpi, BackupKind};
pub use direct_boot::{apply as direct_boot_apply, DirectBootError};
pub use header::{CartHeader, ParseError, HEADER_SIZE};

use serde::{Deserialize, Serialize};

/// Cart state held by the top-level `Nds` struct. The ROM bytes themselves
/// live in `SharedState::slot1_rom` — one copy, excluded from save states
/// (frontends reattach the ROM after a state load).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Cart {
    /// Parsed header (when a ROM is loaded).
    #[serde(default)]
    pub header: Option<CartHeader>,
    /// Size of the loaded ROM in bytes (0 = no cart).
    #[serde(default)]
    pub rom_len: usize,
    // AUXSPI lives in SharedState (it's accessed from the ARM7 I/O page,
    // and the top-level loop wants to fire Slot1 DMA off its events).
}

impl Cart {
    pub fn empty() -> Self {
        Cart::default()
    }

    pub fn header(&self) -> Option<&CartHeader> {
        self.header.as_ref()
    }

    pub fn rom_len(&self) -> Option<usize> {
        (self.rom_len != 0).then_some(self.rom_len)
    }
}

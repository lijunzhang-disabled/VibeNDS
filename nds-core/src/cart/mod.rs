//! Cart slot-1: header parsing and direct-boot loader.
//!
//! AUXSPI backup, KEY1/KEY2 encryption, and the slot-1 command protocol
//! arrive in later phases.

pub mod header;
pub mod direct_boot;

pub use header::{CartHeader, ParseError, HEADER_SIZE};
pub use direct_boot::{apply as direct_boot_apply, DirectBootError};

use serde::{Deserialize, Serialize};

/// Cart state held by the top-level `Nds` struct.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Cart {
    /// Raw ROM bytes. `None` if no cart is loaded.
    #[serde(default)]
    pub rom: Option<Vec<u8>>,
    /// Parsed header (when a ROM is loaded).
    #[serde(default)]
    pub header: Option<CartHeader>,
}

impl Cart {
    pub fn empty() -> Self {
        Cart::default()
    }

    pub fn from_rom(rom: Vec<u8>) -> Result<Self, ParseError> {
        let header = CartHeader::parse(&rom)?;
        Ok(Cart { rom: Some(rom), header: Some(header) })
    }

    pub fn header(&self) -> Option<&CartHeader> { self.header.as_ref() }
    pub fn rom(&self) -> Option<&[u8]> { self.rom.as_deref() }
}

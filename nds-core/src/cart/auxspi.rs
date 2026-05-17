//! AUXSPI — cart-side backup over slot-1's auxiliary SPI bus.
//!
//! Distinct from the main SPI bus that talks to firmware/TSC/PMIC. AUXSPI
//! talks to a single backup chip on the cart, which is one of:
//!
//! | Type | Sizes | Command set |
//! |---|---|---|
//! | EEPROM | 512 B / 8 KB / 64 KB | small: 2-byte addr, large: 3-byte addr |
//! | FRAM | 8 KB / 32 KB | same as EEPROM |
//! | FLASH | 256 KB / 512 KB / 1 MB | NOR-style; needs erase before write |
//!
//! Phase 5 implements EEPROM (all three sizes) and FLASH 256K/512K/1M.
//! FRAM uses the same command set as EEPROM, so it falls out for free.
//!
//! Registers (ARM7 view of slot-1 I/O):
//!
//! ```text
//! 0x040001A0  AUXSPICNT  (16 bit)
//!   [1:0]   baud
//!   [6]     chip-select hold (kept asserted across multiple data bytes)
//!   [7]     busy (RO)
//!   [13]    SPI mode select: 0 = ROM transfer, 1 = SPI backup
//!   [14]    transfer-complete IRQ enable
//!   [15]    slot-1 enable
//!
//! 0x040001A2  AUXSPIDATA (16 bit; low 8 = byte in/out)
//! ```
//!
//! Per GBATEK: bit 13 = 1 selects the *AUXSPI* (backup) path. Bit 13 = 0
//! routes to the slot-1 ROM transfer machine (Phase 5 stubs that out;
//! actual slot-1 cart command protocol is later).

use serde::{Deserialize, Serialize};

/// Backup-chip kind. Stored in the cart's metadata; the AUXSPI command
/// state machine dispatches based on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupKind {
    None,
    Eeprom512B,
    Eeprom8K,
    Eeprom64K,
    Fram32K,
    Flash256K,
    Flash512K,
    Flash1M,
}

impl BackupKind {
    pub fn size(self) -> usize {
        match self {
            BackupKind::None => 0,
            BackupKind::Eeprom512B => 512,
            BackupKind::Eeprom8K => 8 * 1024,
            BackupKind::Eeprom64K => 64 * 1024,
            BackupKind::Fram32K => 32 * 1024,
            BackupKind::Flash256K => 256 * 1024,
            BackupKind::Flash512K => 512 * 1024,
            BackupKind::Flash1M => 1024 * 1024,
        }
    }

    pub fn is_flash(self) -> bool {
        matches!(self, BackupKind::Flash256K | BackupKind::Flash512K | BackupKind::Flash1M)
    }

    /// EEPROM/FRAM address-byte width: 1 byte for 512B, 2 bytes for 8K,
    /// 3 bytes for 64K. FLASH always uses 3 bytes.
    pub fn addr_bytes(self) -> u8 {
        match self {
            BackupKind::Eeprom512B => 1,
            BackupKind::Eeprom8K   => 2,
            BackupKind::Eeprom64K | BackupKind::Fram32K => 3,
            BackupKind::Flash256K | BackupKind::Flash512K | BackupKind::Flash1M => 3,
            BackupKind::None => 0,
        }
    }

    /// Default kind guess from the NDS header's `device_capacity` byte.
    /// Real games are listed in a public database; this heuristic gets us
    /// close enough for most homebrew.
    pub fn guess_from_header(_device_capacity: u8) -> Self {
        // Without a lookup table, default to EEPROM 64K — covers most
        // commercial games. Frontend can override via --save-type.
        BackupKind::Eeprom64K
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Idle,
    /// Reading address bytes after a READ/WRITE command.
    Addr { cmd: u8, remaining: u8, addr: u32 },
    /// Streaming data after address bytes; for READ we return data,
    /// for WRITE we accept it.
    Data { cmd: u8, addr: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuxSpi {
    pub cnt: u16,
    pub data_reg: u8,
    pub kind: BackupKind,

    /// Backup storage. Sized when `kind` is set; empty for `BackupKind::None`.
    #[serde(with = "crate::bus::shared::serde_bytes_vec")]
    pub storage: Vec<u8>,

    /// Status register bits: bit 0 = WIP (write in progress, always 0),
    /// bit 1 = WEL (write enable latch).
    status: u8,
    phase: Phase,
}

impl AuxSpi {
    pub fn new() -> Self {
        AuxSpi {
            cnt: 0,
            data_reg: 0,
            kind: BackupKind::None,
            storage: Vec::new(),
            status: 0,
            phase: Phase::Idle,
        }
    }

    pub fn set_backup_kind(&mut self, kind: BackupKind) {
        self.kind = kind;
        // Pre-fill with 0xFF (erased-flash default; EEPROM games usually
        // detect 0xFF == "no save" too).
        self.storage = vec![0xFFu8; kind.size()];
        self.phase = Phase::Idle;
        self.status = 0;
    }

    pub fn load_save(&mut self, data: &[u8]) {
        let len = data.len().min(self.storage.len());
        self.storage[..len].copy_from_slice(&data[..len]);
    }

    pub fn export_save(&self) -> Option<Vec<u8>> {
        if matches!(self.kind, BackupKind::None) { None }
        else { Some(self.storage.clone()) }
    }

    pub fn read_cnt(&self) -> u16 { self.cnt & !(1 << 7) }

    pub fn write_cnt(&mut self, val: u16) {
        let was_held = self.cnt & (1 << 6) != 0;
        self.cnt = val;
        let now_held = val & (1 << 6) != 0;
        // CS deassert: reset state.
        if was_held && !now_held {
            self.phase = Phase::Idle;
        }
    }

    pub fn read_data(&self) -> u8 { self.data_reg }

    /// Triggered on write to AUXSPIDATA. Returns true if the
    /// transfer-complete IRQ should be raised on ARM7.
    pub fn write_data(&mut self, byte_in: u8) -> bool {
        // Bit 15 = slot enable, bit 13 = SPI-backup mode. Both must be on
        // for the transfer to dispatch to a backup chip.
        let slot_enabled = self.cnt & (1 << 15) != 0;
        let spi_mode = self.cnt & (1 << 13) != 0;
        if !slot_enabled || !spi_mode {
            self.data_reg = 0xFF;
            return self.cnt & (1 << 14) != 0;
        }
        if matches!(self.kind, BackupKind::None) {
            self.data_reg = 0xFF;
            return self.cnt & (1 << 14) != 0;
        }

        self.data_reg = self.handle_byte(byte_in);

        // CS hold: when bit 6 == 0, this is the final byte of the
        // sequence; reset state for the next transaction.
        if self.cnt & (1 << 6) == 0 {
            self.phase = Phase::Idle;
        }

        self.cnt & (1 << 14) != 0
    }

    fn handle_byte(&mut self, byte_in: u8) -> u8 {
        match self.phase {
            Phase::Idle => self.handle_command(byte_in),
            Phase::Addr { cmd, remaining, addr } => {
                let new_addr = (addr << 8) | byte_in as u32;
                if remaining > 1 {
                    self.phase = Phase::Addr { cmd, remaining: remaining - 1, addr: new_addr };
                    0
                } else {
                    self.phase = Phase::Data { cmd, addr: new_addr & ((self.storage.len() as u32).saturating_sub(1)) };
                    0
                }
            }
            Phase::Data { cmd, addr } => self.handle_data(cmd, addr, byte_in),
        }
    }

    fn handle_command(&mut self, cmd: u8) -> u8 {
        let addr_bytes = self.kind.addr_bytes();
        match cmd {
            0x03 => { // READ
                self.phase = Phase::Addr { cmd, remaining: addr_bytes, addr: 0 };
                0
            }
            0x02 => { // WRITE (EEPROM/FRAM); also same opcode as some FLASH chips
                if self.status & 0x02 != 0 {
                    self.phase = Phase::Addr { cmd, remaining: addr_bytes, addr: 0 };
                } else {
                    self.phase = Phase::Idle;
                }
                0
            }
            0x0A if self.kind.is_flash() => { // FLASH PAGE_PROGRAM
                if self.status & 0x02 != 0 {
                    self.phase = Phase::Addr { cmd, remaining: addr_bytes, addr: 0 };
                } else {
                    self.phase = Phase::Idle;
                }
                0
            }
            0xD8 if self.kind.is_flash() => { // FLASH SECTOR_ERASE
                if self.status & 0x02 != 0 {
                    self.phase = Phase::Addr { cmd, remaining: addr_bytes, addr: 0 };
                } else {
                    self.phase = Phase::Idle;
                }
                0
            }
            0x05 => { // READ_STATUS
                self.phase = Phase::Data { cmd, addr: 0 };
                0
            }
            0x06 => { // WRITE_ENABLE
                self.status |= 0x02;
                self.phase = Phase::Idle;
                0
            }
            0x04 => { // WRITE_DISABLE
                self.status &= !0x02;
                self.phase = Phase::Idle;
                0
            }
            0x9F => { // READ_JEDEC_ID (FLASH only; EEPROM ignores)
                self.phase = Phase::Data { cmd, addr: 0 };
                0
            }
            _ => {
                log::trace!("AUXSPI: unhandled cmd 0x{:02X} for kind {:?}", cmd, self.kind);
                self.phase = Phase::Idle;
                0xFF
            }
        }
    }

    fn handle_data(&mut self, cmd: u8, addr: u32, byte_in: u8) -> u8 {
        match cmd {
            0x03 => {
                let byte = self.storage.get(addr as usize).copied().unwrap_or(0xFF);
                self.phase = Phase::Data { cmd, addr: addr.wrapping_add(1) };
                byte
            }
            0x02 | 0x0A => { // EEPROM WRITE or FLASH PAGE_PROGRAM
                if let Some(slot) = self.storage.get_mut(addr as usize) {
                    if cmd == 0x0A {
                        // FLASH page-program AND-masks against existing data
                        // (flash cells only flip 1→0 without an erase).
                        *slot &= byte_in;
                    } else {
                        *slot = byte_in;
                    }
                }
                self.phase = Phase::Data { cmd, addr: addr.wrapping_add(1) };
                0
            }
            0xD8 => { // FLASH SECTOR_ERASE — fires once on first data byte
                let base = (addr as usize) & !0xFFF; // 4 KB sectors
                let end = (base + 0x1000).min(self.storage.len());
                for b in &mut self.storage[base..end] {
                    *b = 0xFF;
                }
                self.status &= !0x02;
                self.phase = Phase::Idle;
                0
            }
            0x05 => {
                let s = self.status;
                self.phase = Phase::Data { cmd, addr: 0 };
                s
            }
            0x9F => {
                let id: [u8; 3] = [0xC2, 0x11, 0x05]; // Macronix-style placeholder
                let idx = (addr as usize) % 3;
                self.phase = Phase::Data { cmd, addr: addr.wrapping_add(1) };
                id[idx]
            }
            _ => 0xFF,
        }
    }
}

impl Default for AuxSpi {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(aux: &mut AuxSpi, bytes: &[u8]) -> Vec<u8> {
        // Set master enable + SPI mode + CS-hold across all but the last byte.
        let base_cnt = (1 << 15) | (1 << 13);
        let mut out = Vec::with_capacity(bytes.len());
        for (i, &b) in bytes.iter().enumerate() {
            let hold = i + 1 < bytes.len();
            aux.cnt = base_cnt | if hold { 1 << 6 } else { 0 };
            let _ = aux.write_data(b);
            out.push(aux.read_data());
        }
        out
    }

    #[test]
    fn test_eeprom_64k_read_write_round_trip() {
        let mut aux = AuxSpi::new();
        aux.set_backup_kind(BackupKind::Eeprom64K);

        // WRITE_ENABLE
        issue(&mut aux, &[0x06]);
        // WRITE: cmd 0x02, 3-byte addr 0x000100, then data bytes 0x42 0x99
        issue(&mut aux, &[0x02, 0x00, 0x01, 0x00, 0x42, 0x99]);
        // READ back: cmd 0x03, addr 0x000100, two data bytes
        let out = issue(&mut aux, &[0x03, 0x00, 0x01, 0x00, 0, 0]);
        // First 4 bytes are command + address (return 0); last 2 are data.
        assert_eq!(out[4], 0x42);
        assert_eq!(out[5], 0x99);
    }

    #[test]
    fn test_eeprom_write_requires_wel() {
        let mut aux = AuxSpi::new();
        aux.set_backup_kind(BackupKind::Eeprom64K);
        // Skip WRITE_ENABLE; write should silently fail.
        issue(&mut aux, &[0x02, 0x00, 0x10, 0x00, 0xAA]);
        let out = issue(&mut aux, &[0x03, 0x00, 0x10, 0x00, 0]);
        // Storage is 0xFF default — read back should still be 0xFF.
        assert_eq!(out[4], 0xFF);
    }

    #[test]
    fn test_flash_1m_page_program_and_erase() {
        let mut aux = AuxSpi::new();
        aux.set_backup_kind(BackupKind::Flash1M);

        // Mark a sector with known data — fill via WEL + PAGE_PROGRAM.
        issue(&mut aux, &[0x06]); // WEL
        issue(&mut aux, &[0x0A, 0x00, 0x20, 0x00, 0x55, 0x55, 0x55, 0x55]);
        let out = issue(&mut aux, &[0x03, 0x00, 0x20, 0x00, 0, 0, 0, 0]);
        for i in 0..4 {
            assert_eq!(out[4 + i], 0x55);
        }

        // Erase the sector containing 0x002000.
        issue(&mut aux, &[0x06]);
        issue(&mut aux, &[0xD8, 0x00, 0x20, 0x00, 0]);
        // The erase fires on the first data byte (the trailing 0). All 4 KB
        // around 0x002000 should now be 0xFF.
        let out = issue(&mut aux, &[0x03, 0x00, 0x20, 0x00, 0, 0, 0, 0]);
        for i in 0..4 {
            assert_eq!(out[4 + i], 0xFF);
        }
    }

    #[test]
    fn test_read_status() {
        let mut aux = AuxSpi::new();
        aux.set_backup_kind(BackupKind::Eeprom64K);
        issue(&mut aux, &[0x06]); // WRITE_ENABLE
        let out = issue(&mut aux, &[0x05, 0]);
        assert_eq!(out[1] & 0x02, 0x02, "WEL bit should be set after 0x06");
    }

    #[test]
    fn test_export_import_save_round_trip() {
        let mut aux = AuxSpi::new();
        aux.set_backup_kind(BackupKind::Eeprom8K);
        issue(&mut aux, &[0x06]);
        issue(&mut aux, &[0x02, 0x00, 0x10, 0xDE, 0xAD, 0xBE, 0xEF]);

        let sav = aux.export_save().expect("save bytes");
        assert_eq!(sav.len(), 8 * 1024);
        assert_eq!(&sav[0x10..0x14], &[0xDE, 0xAD, 0xBE, 0xEF]);

        // Round-trip through a fresh AuxSpi.
        let mut aux2 = AuxSpi::new();
        aux2.set_backup_kind(BackupKind::Eeprom8K);
        aux2.load_save(&sav);
        let out = issue(&mut aux2, &[0x03, 0x00, 0x10, 0, 0, 0, 0]);
        assert_eq!(&out[3..7], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }
}

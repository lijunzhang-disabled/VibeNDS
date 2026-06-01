//! NDS cart header parser.
//!
//! Layout per GBATEK §"DS Cartridge Header". The header is 0x200 bytes
//! and lives at the start of every `.nds` file.

use serde::{Deserialize, Serialize};

pub const HEADER_SIZE: usize = 0x200;

/// Subset of the 0x200-byte header that we care about for direct boot.
/// Other fields (overlay tables, secure area checksum, etc.) are added
/// in later phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartHeader {
    pub title: String,
    pub gamecode: [u8; 4],
    pub makercode: [u8; 2],
    pub unit_code: u8,
    pub device_capacity: u8,
    pub region: u8,
    pub rom_version: u8,

    pub arm9_rom_offset: u32,
    pub arm9_entry: u32,
    pub arm9_load: u32,
    pub arm9_size: u32,

    pub arm7_rom_offset: u32,
    pub arm7_entry: u32,
    pub arm7_load: u32,
    pub arm7_size: u32,

    pub fnt_offset: u32,
    pub fnt_size: u32,
    pub fat_offset: u32,
    pub fat_size: u32,

    pub icon_offset: u32,
    pub total_used_rom: u32,
    pub header_size: u32,

    pub header_crc: u16,
    pub computed_header_crc: u16,
}

impl CartHeader {
    /// Parse a header from a ROM byte slice. The slice must be at least
    /// 0x200 bytes.
    pub fn parse(rom: &[u8]) -> Result<Self, ParseError> {
        if rom.len() < HEADER_SIZE {
            return Err(ParseError::TooShort(rom.len()));
        }

        let read_u32 = |off: usize| -> u32 {
            u32::from_le_bytes([rom[off], rom[off + 1], rom[off + 2], rom[off + 3]])
        };
        let read_u16 = |off: usize| -> u16 { u16::from_le_bytes([rom[off], rom[off + 1]]) };

        let title = String::from_utf8_lossy(&rom[0x00..0x0C])
            .trim_end_matches('\0')
            .to_string();
        let mut gamecode = [0u8; 4];
        gamecode.copy_from_slice(&rom[0x0C..0x10]);
        let mut makercode = [0u8; 2];
        makercode.copy_from_slice(&rom[0x10..0x12]);

        let header_crc = read_u16(0x15E);
        let computed_header_crc = crc16_modbus(&rom[..0x15E]);

        Ok(CartHeader {
            title,
            gamecode,
            makercode,
            unit_code: rom[0x12],
            device_capacity: rom[0x14],
            region: rom[0x1D],
            rom_version: rom[0x1E],

            arm9_rom_offset: read_u32(0x20),
            arm9_entry: read_u32(0x24),
            arm9_load: read_u32(0x28),
            arm9_size: read_u32(0x2C),

            arm7_rom_offset: read_u32(0x30),
            arm7_entry: read_u32(0x34),
            arm7_load: read_u32(0x38),
            arm7_size: read_u32(0x3C),

            fnt_offset: read_u32(0x40),
            fnt_size: read_u32(0x44),
            fat_offset: read_u32(0x48),
            fat_size: read_u32(0x4C),

            icon_offset: read_u32(0x68),
            total_used_rom: read_u32(0x80),
            header_size: read_u32(0x84),

            header_crc,
            computed_header_crc,
        })
    }

    /// `true` if the parsed header CRC matches the value we computed over
    /// bytes 0..0x15E. Real ROMs satisfy this; homebrew sometimes doesn't.
    pub fn header_crc_valid(&self) -> bool {
        self.header_crc == self.computed_header_crc
    }

    /// Game code as a printable string (e.g. `"AKWE"` for Pokémon Mystery
    /// Dungeon Red). Falls back to hex for non-ASCII.
    pub fn gamecode_str(&self) -> String {
        if self.gamecode.iter().all(|b| b.is_ascii_graphic()) {
            String::from_utf8_lossy(&self.gamecode).into_owned()
        } else {
            format!("0x{:08X}", u32::from_be_bytes(self.gamecode))
        }
    }
}

#[derive(Debug)]
pub enum ParseError {
    /// The slice was shorter than the 0x200-byte header.
    TooShort(usize),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::TooShort(len) => {
                write!(
                    f,
                    "ROM too short: {} bytes, header needs {}",
                    len, HEADER_SIZE
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// CRC-16/MODBUS — the algorithm GBATEK calls "CRC-16" for the header
/// checksum. Polynomial 0xA001 (reflected 0x8005), init 0xFFFF, no final
/// XOR.
pub fn crc16_modbus(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_header() -> Vec<u8> {
        let mut rom = vec![0u8; HEADER_SIZE];
        rom[0x00..0x0C].copy_from_slice(b"PHASE2 TEST\0".as_ref());
        rom[0x0C..0x10].copy_from_slice(b"AAAE");
        rom[0x10..0x12].copy_from_slice(b"01");
        rom[0x12] = 0;
        rom[0x14] = 0x07; // 16 MB device capacity
        rom[0x1D] = 0;
        rom[0x1E] = 0;

        // ARM9: rom_offset=0x4000, entry=0x02000800, load=0x02000000, size=0x100
        rom[0x20..0x24].copy_from_slice(&0x4000u32.to_le_bytes());
        rom[0x24..0x28].copy_from_slice(&0x0200_0800u32.to_le_bytes());
        rom[0x28..0x2C].copy_from_slice(&0x0200_0000u32.to_le_bytes());
        rom[0x2C..0x30].copy_from_slice(&0x100u32.to_le_bytes());

        // ARM7: rom_offset=0x4200, entry=0x02380000, load=0x02380000, size=0x80
        rom[0x30..0x34].copy_from_slice(&0x4200u32.to_le_bytes());
        rom[0x34..0x38].copy_from_slice(&0x0238_0000u32.to_le_bytes());
        rom[0x38..0x3C].copy_from_slice(&0x0238_0000u32.to_le_bytes());
        rom[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());

        // Header size + total ROM
        rom[0x80..0x84].copy_from_slice(&0x4280u32.to_le_bytes());
        rom[0x84..0x88].copy_from_slice(&0x4000u32.to_le_bytes());

        // Compute and stamp header CRC at 0x15E
        let crc = crc16_modbus(&rom[..0x15E]);
        rom[0x15E..0x160].copy_from_slice(&crc.to_le_bytes());
        rom
    }

    #[test]
    fn test_parse_basic_header() {
        let rom = synth_header();
        let h = CartHeader::parse(&rom).expect("parse");
        assert_eq!(h.title, "PHASE2 TEST");
        assert_eq!(h.gamecode, *b"AAAE");
        assert_eq!(h.makercode, *b"01");
        assert_eq!(h.arm9_rom_offset, 0x4000);
        assert_eq!(h.arm9_entry, 0x0200_0800);
        assert_eq!(h.arm9_load, 0x0200_0000);
        assert_eq!(h.arm9_size, 0x100);
        assert_eq!(h.arm7_rom_offset, 0x4200);
        assert_eq!(h.arm7_load, 0x0238_0000);
        assert_eq!(h.arm7_size, 0x80);
    }

    #[test]
    fn test_header_crc_valid() {
        let rom = synth_header();
        let h = CartHeader::parse(&rom).expect("parse");
        assert!(
            h.header_crc_valid(),
            "synthetic header should have a valid CRC; got 0x{:04X} vs computed 0x{:04X}",
            h.header_crc,
            h.computed_header_crc
        );
    }

    #[test]
    fn test_header_crc_invalid_when_byte_flipped() {
        let mut rom = synth_header();
        rom[0x10] = b'X'; // corrupt makercode
        let h = CartHeader::parse(&rom).expect("parse");
        assert!(!h.header_crc_valid());
    }

    #[test]
    fn test_too_short_rom() {
        let rom = vec![0u8; 0x100];
        assert!(matches!(
            CartHeader::parse(&rom),
            Err(ParseError::TooShort(_))
        ));
    }

    #[test]
    fn test_crc16_modbus_known_vector() {
        // Test vector: "123456789" → 0x4B37
        assert_eq!(crc16_modbus(b"123456789"), 0x4B37);
    }
}

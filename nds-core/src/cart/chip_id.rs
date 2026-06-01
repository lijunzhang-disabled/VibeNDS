//! Slot-1 ROM chip-ID helpers.

/// Return the plain little-endian ROM chip ID used by boot indicators and
/// unencrypted command probes. KEY2-encrypted responses are layered on top of
/// this value once the encrypted card protocol is implemented.
pub fn chip_id_for_rom(rom: &[u8]) -> u32 {
    let mb = ((rom.len() + 0x0F_FFFF) / 0x10_0000).max(1);
    let size_byte = if mb <= 0x80 {
        (mb - 1) as u8
    } else {
        // GBATEK documents the first range as (N+1) MB, which covers all
        // normal NDS ROM sizes up to 128 MB. Clamp larger images until the
        // encrypted/newer-card protocol is modeled more accurately.
        0x7F
    };

    let mut flags2 = 0u8;
    let mut flags3 = if mb >= 0x40 { 0x80 } else { 0x00 };

    let manufacturer = if has_ir(rom) {
        flags2 |= 0x01;
        flags3 |= 0xE0;
        0x80
    } else {
        0xC2
    };

    u32::from_le_bytes([manufacturer, size_byte, flags2, flags3])
}

fn has_ir(rom: &[u8]) -> bool {
    let Some(gamecode) = rom.get(0x0C..0x10) else {
        return false;
    };

    matches!(
        gamecode,
        // Pokemon HeartGold/SoulSilver.
        b"IPKJ" | b"IPKE" | b"IPKP" | b"IPGJ" | b"IPGE" | b"IPGP"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_id_reflects_small_rom_size() {
        let rom = vec![0; 16 * 1024 * 1024];

        assert_eq!(chip_id_for_rom(&rom), 0x0000_0FC2);
    }

    #[test]
    fn chip_id_marks_large_roms_as_new_protocol() {
        let rom = vec![0; 128 * 1024 * 1024];

        assert_eq!(chip_id_for_rom(&rom), 0x8000_7FC2);
    }

    #[test]
    fn chip_id_marks_heartgold_as_ir_cart() {
        let mut rom = vec![0; 128 * 1024 * 1024];
        rom[0x0C..0x10].copy_from_slice(b"IPKE");

        assert_eq!(chip_id_for_rom(&rom), 0xE001_7F80);
    }
}

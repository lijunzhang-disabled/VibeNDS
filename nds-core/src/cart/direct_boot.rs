//! Direct-boot path. Emulates the firmware's RAM-loader stage: copies the
//! ARM9 and ARM7 binaries from the cart into Main RAM and sets up CPU
//! state so each core can fall straight into its entry point.
//!
//! Reference: GBATEK §"DS Cartridge Header" + §"DS Direct Boot".

use crate::bus::{Arm7Memory, SharedState};
use crate::cpu::{Cpu, CpuMode, Psr};
use super::header::CartHeader;

/// Standard ARM9 stack pointers per the NDS BIOS direct-boot convention.
pub const ARM9_SP_USR: u32 = 0x0300_2F7C;
pub const ARM9_SP_IRQ: u32 = 0x0300_3F80;
pub const ARM9_SP_SVC: u32 = 0x0300_3FC0;

/// Standard ARM7 stack pointers — these point into ARM7 WRAM at 0x03800000.
pub const ARM7_SP_USR: u32 = 0x0380_FD80;
pub const ARM7_SP_IRQ: u32 = 0x0380_FF80;
pub const ARM7_SP_SVC: u32 = 0x0380_FFC0;

/// Where the firmware copies the cart header inside Main RAM. Many games
/// read fields out of this block instead of out of the cart bus.
pub const HEADER_COPY_ADDR: u32 = 0x027F_FE00;

/// Boot indicators block. The BIOS writes a handful of well-known values
/// here for games to read.
pub const BOOT_INDICATORS_BASE: u32 = 0x027F_F800;

/// Apply direct-boot setup to both CPUs and `SharedState` from a parsed
/// header + the raw ROM bytes.
pub fn apply(
    cpu9: &mut Cpu,
    cpu7: &mut Cpu,
    mem7: &mut Arm7Memory,
    shared: &mut SharedState,
    header: &CartHeader,
    rom: &[u8],
) -> Result<(), DirectBootError> {
    copy_binary(shared, mem7, rom, header.arm9_rom_offset, header.arm9_load, header.arm9_size, "ARM9")?;
    copy_binary(shared, mem7, rom, header.arm7_rom_offset, header.arm7_load, header.arm7_size, "ARM7")?;
    copy_header_into_ram(shared, rom);
    write_boot_indicators(shared, header);

    setup_arm9(cpu9, header.arm9_entry);
    setup_arm7(cpu7, header.arm7_entry);

    // Direct boot leaves WRAMCNT in mode 3 (ARM7 owns the full shared WRAM).
    // Most games set it themselves shortly after boot anyway.
    shared.wramcnt = 3;

    // POSTFLG starts at 1 on direct boot (BIOS finished). We don't model
    // POSTFLG yet — a placeholder for Phase 2.

    Ok(())
}

/// Map a 0x02xxxxxx address to the physical offset inside the 4 MB Main RAM,
/// honoring the standard 4-MB mirror window (0x02000000..0x02FFFFFF).
fn main_ram_offset(addr: u32) -> usize {
    (addr & 0x003F_FFFF) as usize
}

fn arm7_wram_offset(addr: u32) -> usize {
    (addr & 0x0000_FFFF) as usize
}

fn copy_binary(
    shared: &mut SharedState,
    mem7: &mut Arm7Memory,
    rom: &[u8],
    rom_offset: u32,
    load_addr: u32,
    size: u32,
    label: &'static str,
) -> Result<(), DirectBootError> {
    let src_start = rom_offset as usize;
    let src_end = src_start.checked_add(size as usize)
        .ok_or(DirectBootError::ArithmeticOverflow)?;
    if src_end > rom.len() {
        return Err(DirectBootError::OutOfRangeRom { label, src_end, rom_len: rom.len() });
    }

    if (load_addr >> 24) == 0x02 {
        let dst_off = main_ram_offset(load_addr);
        if dst_off + size as usize > shared.main_ram.len() {
            return Err(DirectBootError::UnsupportedLoadRegion { label, addr: load_addr, size });
        }

        shared.main_ram[dst_off..dst_off + size as usize]
            .copy_from_slice(&rom[src_start..src_end]);
    } else if label == "ARM7" && (load_addr >> 24) == 0x03 && load_addr >= 0x0380_0000 {
        let dst_off = arm7_wram_offset(load_addr);
        if dst_off + size as usize > mem7.wram.len() {
            return Err(DirectBootError::UnsupportedLoadRegion { label, addr: load_addr, size });
        }

        mem7.wram[dst_off..dst_off + size as usize]
            .copy_from_slice(&rom[src_start..src_end]);
    } else {
        return Err(DirectBootError::UnsupportedLoadRegion { label, addr: load_addr, size });
    }
    Ok(())
}

fn copy_header_into_ram(shared: &mut SharedState, rom: &[u8]) {
    let off = main_ram_offset(HEADER_COPY_ADDR);
    let copy_len = rom.len().min(super::header::HEADER_SIZE);
    shared.main_ram[off..off + copy_len].copy_from_slice(&rom[..copy_len]);
}

/// Write the boot indicator words at `0x027FF800-0x027FFC00` that real DS
/// firmware leaves behind. Only the ones games actually read are populated.
fn write_boot_indicators(shared: &mut SharedState, header: &CartHeader) {
    let chip_id: u32 = 0x0000_00C2;
    let off = main_ram_offset(0x027F_F800);
    shared.main_ram[off..off + 4].copy_from_slice(&chip_id.to_le_bytes());
    // Mirror at 0x027FF804
    shared.main_ram[off + 4..off + 8].copy_from_slice(&chip_id.to_le_bytes());

    // 0x027FF808: secure-area CRC and OK flag (skip-check value 0xFFFF) — games
    // verifying the secure area should accept this when KEY1 isn't enforced.
    shared.main_ram[main_ram_offset(0x027F_F808)..main_ram_offset(0x027F_F808) + 2]
        .copy_from_slice(&0xFFFFu16.to_le_bytes());

    // 0x027FF850: ARM7 BIOS CRC (we don't compute, leave 0)

    // 0x027FF860: chip ID at boot, again
    shared.main_ram[main_ram_offset(0x027F_F860)..main_ram_offset(0x027F_F860) + 4]
        .copy_from_slice(&chip_id.to_le_bytes());

    // 0x027FF864-0x027FF868: header CRC + secure-area CRC
    shared.main_ram[main_ram_offset(0x027F_F864)..main_ram_offset(0x027F_F864) + 2]
        .copy_from_slice(&header.header_crc.to_le_bytes());

    // 0x027FF880: message-from-ARM9-to-ARM7 (zero)
    // 0x027FF884: ARM7 BOOT-status (zero — "still booting", will be set)
    // 0x027FFC40: boot-indicator (1 = direct-boot)
    shared.main_ram[main_ram_offset(0x027F_FC40)] = 1;
}

fn setup_arm9(cpu: &mut Cpu, entry: u32) {
    cpu.cpsr = Psr::new(CpuMode::System);
    cpu.cpsr.bits &= !(1 << 7); // IRQ enabled
    cpu.cpsr.bits &= !(1 << 6); // FIQ enabled
    // Homebrew startup code expects ITCM to exist before entering ARM9 code.
    // Some test ROMs expand this into a larger low-address mirror themselves.
    cpu.cp15.write(9, 1, 0, 1, (6 << 1) | 1);
    cpu.regs[13] = ARM9_SP_USR;
    cpu.banked.sp[CpuMode::Irq.bank_index()] = ARM9_SP_IRQ;
    cpu.banked.sp[CpuMode::Supervisor.bank_index()] = ARM9_SP_SVC;
    cpu.regs[15] = entry;
    cpu.pipeline_flushed = true;
}

fn setup_arm7(cpu: &mut Cpu, entry: u32) {
    cpu.cpsr = Psr::new(CpuMode::System);
    cpu.cpsr.bits &= !(1 << 7);
    cpu.cpsr.bits &= !(1 << 6);
    cpu.regs[13] = ARM7_SP_USR;
    cpu.banked.sp[CpuMode::Irq.bank_index()] = ARM7_SP_IRQ;
    cpu.banked.sp[CpuMode::Supervisor.bank_index()] = ARM7_SP_SVC;
    cpu.regs[15] = entry;
    cpu.pipeline_flushed = true;
}

#[derive(Debug)]
pub enum DirectBootError {
    ArithmeticOverflow,
    OutOfRangeRom { label: &'static str, src_end: usize, rom_len: usize },
    UnsupportedLoadRegion { label: &'static str, addr: u32, size: u32 },
}

impl std::fmt::Display for DirectBootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirectBootError::ArithmeticOverflow => write!(f, "arithmetic overflow during direct boot"),
            DirectBootError::OutOfRangeRom { label, src_end, rom_len } =>
                write!(f, "{} binary extends past ROM end (needs 0x{:X}, ROM is 0x{:X})", label, src_end, rom_len),
            DirectBootError::UnsupportedLoadRegion { label, addr, size } =>
                write!(f, "{} load region 0x{:08X}+0x{:X} not in supported RAM", label, addr, size),
        }
    }
}

impl std::error::Error for DirectBootError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::header::{crc16_modbus, HEADER_SIZE};

    fn synth_rom() -> (Vec<u8>, CartHeader) {
        let mut rom = vec![0u8; 0x4400];
        rom[0x00..0x0C].copy_from_slice(b"DBOOT TEST\0\0".as_ref());
        rom[0x0C..0x10].copy_from_slice(b"AAAE");
        rom[0x10..0x12].copy_from_slice(b"01");

        // ARM9 binary: just a B . at 0x4000 → entry 0x02000000, load 0x02000000, size 4
        rom[0x4000..0x4004].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
        rom[0x20..0x24].copy_from_slice(&0x4000u32.to_le_bytes());
        rom[0x24..0x28].copy_from_slice(&0x0200_0000u32.to_le_bytes());
        rom[0x28..0x2C].copy_from_slice(&0x0200_0000u32.to_le_bytes());
        rom[0x2C..0x30].copy_from_slice(&4u32.to_le_bytes());

        // ARM7 binary: also B . at 0x4200 → entry 0x02380000
        rom[0x4200..0x4204].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
        rom[0x30..0x34].copy_from_slice(&0x4200u32.to_le_bytes());
        rom[0x34..0x38].copy_from_slice(&0x0238_0000u32.to_le_bytes());
        rom[0x38..0x3C].copy_from_slice(&0x0238_0000u32.to_le_bytes());
        rom[0x3C..0x40].copy_from_slice(&4u32.to_le_bytes());

        let crc = crc16_modbus(&rom[..0x15E]);
        rom[0x15E..0x160].copy_from_slice(&crc.to_le_bytes());

        let h = CartHeader::parse(&rom).expect("parse");
        assert_eq!(rom.len(), 0x4400);
        let _ = HEADER_SIZE;
        (rom, h)
    }

    #[test]
    fn test_direct_boot_loads_arm9_arm7_to_main_ram() {
        let (rom, header) = synth_rom();
        let mut cpu9 = Cpu::new_arm9();
        let mut cpu7 = Cpu::new_arm7();
        let mut mem7 = Arm7Memory::new(None);
        let mut shared = SharedState::new();

        apply(&mut cpu9, &mut cpu7, &mut mem7, &mut shared, &header, &rom).expect("direct boot");

        // ARM9 binary at 0x02000000 — first 4 bytes are the B . opcode
        assert_eq!(&shared.main_ram[0..4], &0xEAFF_FFFEu32.to_le_bytes());
        // ARM7 binary at 0x02380000 — that's 0x02380000 - 0x02000000 = 0x380000
        assert_eq!(&shared.main_ram[0x380000..0x380004], &0xEAFF_FFFEu32.to_le_bytes());
    }

    #[test]
    fn test_direct_boot_loads_arm7_to_private_wram() {
        let (rom, mut header) = synth_rom();
        header.arm7_load = 0x0380_0000;
        header.arm7_entry = 0x0380_0000;
        let mut cpu9 = Cpu::new_arm9();
        let mut cpu7 = Cpu::new_arm7();
        let mut mem7 = Arm7Memory::new(None);
        let mut shared = SharedState::new();

        apply(&mut cpu9, &mut cpu7, &mut mem7, &mut shared, &header, &rom).expect("direct boot");

        assert_eq!(&mem7.wram[0..4], &0xEAFF_FFFEu32.to_le_bytes());
        assert_eq!(cpu7.regs[15], 0x0380_0000);
    }

    #[test]
    fn test_direct_boot_sets_pcs_and_sps() {
        let (rom, header) = synth_rom();
        let mut cpu9 = Cpu::new_arm9();
        let mut cpu7 = Cpu::new_arm7();
        let mut mem7 = Arm7Memory::new(None);
        let mut shared = SharedState::new();

        apply(&mut cpu9, &mut cpu7, &mut mem7, &mut shared, &header, &rom).expect("direct boot");

        assert_eq!(cpu9.regs[15], 0x0200_0000);
        assert_eq!(cpu9.regs[13], ARM9_SP_USR);
        assert_eq!(cpu9.banked.sp[CpuMode::Irq.bank_index()], ARM9_SP_IRQ);
        assert_eq!(cpu9.banked.sp[CpuMode::Supervisor.bank_index()], ARM9_SP_SVC);
        assert_eq!(cpu9.cp15.itcm.size_bytes, 32 * 1024);

        assert_eq!(cpu7.regs[15], 0x0238_0000);
        assert_eq!(cpu7.regs[13], ARM7_SP_USR);
        assert_eq!(cpu7.banked.sp[CpuMode::Irq.bank_index()], ARM7_SP_IRQ);
        assert_eq!(cpu7.banked.sp[CpuMode::Supervisor.bank_index()], ARM7_SP_SVC);
    }

    #[test]
    fn test_direct_boot_copies_header_into_ram() {
        let (rom, header) = synth_rom();
        let mut cpu9 = Cpu::new_arm9();
        let mut cpu7 = Cpu::new_arm7();
        let mut mem7 = Arm7Memory::new(None);
        let mut shared = SharedState::new();

        apply(&mut cpu9, &mut cpu7, &mut mem7, &mut shared, &header, &rom).expect("direct boot");

        let off = (HEADER_COPY_ADDR & 0x003F_FFFF) as usize;
        assert_eq!(&shared.main_ram[off..off + 12], b"DBOOT TEST\0\0");
        let crc_in_copy = u16::from_le_bytes([
            shared.main_ram[off + 0x15E], shared.main_ram[off + 0x15F]
        ]);
        assert_eq!(crc_in_copy, header.header_crc);
    }

    #[test]
    fn test_direct_boot_writes_boot_indicators() {
        let (rom, header) = synth_rom();
        let mut cpu9 = Cpu::new_arm9();
        let mut cpu7 = Cpu::new_arm7();
        let mut mem7 = Arm7Memory::new(None);
        let mut shared = SharedState::new();

        apply(&mut cpu9, &mut cpu7, &mut mem7, &mut shared, &header, &rom).expect("direct boot");

        let off = |a: u32| (a & 0x003F_FFFF) as usize;
        let chip_id_a = u32::from_le_bytes([
            shared.main_ram[off(0x027F_F800)],
            shared.main_ram[off(0x027F_F800) + 1],
            shared.main_ram[off(0x027F_F800) + 2],
            shared.main_ram[off(0x027F_F800) + 3],
        ]);
        assert_eq!(chip_id_a, 0x0000_00C2);
        assert_eq!(shared.main_ram[off(0x027F_FC40)], 1);
    }

    #[test]
    fn test_direct_boot_rejects_out_of_range_binary() {
        let (mut rom, mut header) = synth_rom();
        // Make ARM9 binary claim to be 0x10000 bytes — past ROM end.
        header.arm9_size = 0x10000;
        let mut cpu9 = Cpu::new_arm9();
        let mut cpu7 = Cpu::new_arm7();
        let mut mem7 = Arm7Memory::new(None);
        let mut shared = SharedState::new();

        let _ = &mut rom;
        let result = apply(&mut cpu9, &mut cpu7, &mut mem7, &mut shared, &header, &rom);
        assert!(matches!(result, Err(DirectBootError::OutOfRangeRom { .. })));
    }
}

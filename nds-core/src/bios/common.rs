//! BIOS HLE routines shared between ARM9 and ARM7. SWIs that exist on
//! both CPUs with identical (or near-identical) semantics live here.

use crate::cpu::{Cpu, CpuBus};

/// NDS SWI 0x09 — Div: signed 32-bit divide.
/// In: R0 = numerator, R1 = denominator.
/// Out: R0 = quotient, R1 = remainder, R3 = abs(quotient).
pub fn swi_div(cpu: &mut Cpu) {
    let num = cpu.regs[0] as i32;
    let den = cpu.regs[1] as i32;
    if den == 0 {
        // Hardware: leaves results unchanged on /0; we mirror that.
        return;
    }
    let q = num.wrapping_div(den);
    let r = num.wrapping_rem(den);
    cpu.regs[0] = q as u32;
    cpu.regs[1] = r as u32;
    cpu.regs[3] = q.unsigned_abs();
}

/// GBA/compatibility DivArm helper: same as Div, but with R0/R1 input order reversed.
pub fn swi_div_arm(cpu: &mut Cpu) {
    cpu.regs.swap(0, 1);
    swi_div(cpu);
}

/// NDS SWI 0x0D — Sqrt: integer square root of R0.
pub fn swi_sqrt(cpu: &mut Cpu) {
    let n = cpu.regs[0];
    cpu.regs[0] = (n as f64).sqrt() as u32;
}

/// SWI 0x0B — CpuSet: copy or fill memory.
/// In: R0 = source, R1 = dest, R2 = control word.
///   bits[20:0] = element count, bit 24 = fill (0=copy, 1=fill),
///   bit 26 = transfer width (0=halfword, 1=word).
pub fn swi_cpu_set<B: CpuBus>(cpu: &mut Cpu, bus: &mut B) {
    let mut src = cpu.regs[0];
    let mut dst = cpu.regs[1];
    let ctrl = cpu.regs[2];
    let count = ctrl & 0x001F_FFFF;
    let fill = ctrl & (1 << 24) != 0;
    let word_xfer = ctrl & (1 << 26) != 0;

    if word_xfer {
        let value = if fill { bus.read32(src) } else { 0 };
        for _ in 0..count {
            let v = if fill { value } else { bus.read32(src) };
            bus.write32(dst, v);
            if !fill { src = src.wrapping_add(4); }
            dst = dst.wrapping_add(4);
        }
    } else {
        let value = if fill { bus.read16(src) } else { 0 };
        for _ in 0..count {
            let v = if fill { value } else { bus.read16(src) };
            bus.write16(dst, v);
            if !fill { src = src.wrapping_add(2); }
            dst = dst.wrapping_add(2);
        }
    }
}

/// SWI 0x0C — CpuFastSet: 32-bit copy/fill, 8-word blocks.
/// Same encoding as CpuSet but always word-transfer; count is rounded up to
/// a multiple of 8 internally.
pub fn swi_cpu_fast_set<B: CpuBus>(cpu: &mut Cpu, bus: &mut B) {
    let mut src = cpu.regs[0];
    let mut dst = cpu.regs[1];
    let ctrl = cpu.regs[2];
    let mut count = ctrl & 0x001F_FFFF;
    let fill = ctrl & (1 << 24) != 0;

    // Round count up to a multiple of 8.
    count = (count + 7) & !7;

    if fill {
        let value = bus.read32(src);
        for _ in 0..count {
            bus.write32(dst, value);
            dst = dst.wrapping_add(4);
        }
    } else {
        for _ in 0..count {
            let v = bus.read32(src);
            bus.write32(dst, v);
            src = src.wrapping_add(4);
            dst = dst.wrapping_add(4);
        }
    }
}

/// SWI 0x0D — GetCRC16: compute CRC-16 over a memory range.
/// In: R0 = initial CRC, R1 = source pointer, R2 = byte length.
/// Out: R0 = CRC, R3 = source + length.
pub fn swi_get_crc16<B: CpuBus>(cpu: &mut Cpu, bus: &mut B) {
    let mut crc = cpu.regs[0] as u16;
    let mut ptr = cpu.regs[1];
    let len = cpu.regs[2];
    // Halfword-aligned, big-endian-ish polynomial 0xA001 — same as MODBUS.
    for _ in 0..(len / 2) {
        let val = bus.read16(ptr);
        for byte in val.to_le_bytes() {
            crc ^= byte as u16;
            for _ in 0..8 {
                if crc & 1 != 0 { crc = (crc >> 1) ^ 0xA001; } else { crc >>= 1; }
            }
        }
        ptr = ptr.wrapping_add(2);
    }
    cpu.regs[0] = crc as u32;
    cpu.regs[3] = cpu.regs[1].wrapping_add(len);
}

/// SWI 0x11 / 0x12 — LZ77UnComp{Wram,Vram}.
///
/// LZ77 stream:
///   header (4 bytes): [31:8]=decompressed size, [7:4]=type (1=LZ77), [3:0]=reserved.
///   body: groups of (1 flag byte) + 8 entries.
///     flag bit = 0 → next byte is a literal.
///     flag bit = 1 → next 2 bytes are an LZ ref:
///       byte0[7:4]=length-3 (0..15 maps to 3..18), byte0[3:0]+byte1=disp-1.
pub fn swi_lz77_uncomp<B: CpuBus>(cpu: &mut Cpu, bus: &mut B, vram: bool) {
    let mut src = cpu.regs[0];
    let mut dst = cpu.regs[1];

    let header = bus.read32(src);
    src = src.wrapping_add(4);
    let total = header >> 8;
    let mut written = 0u32;

    // VRAM variant must use halfword writes (no 8-bit access). We accumulate
    // byte pairs and flush via write16; the WRAM variant just uses write8.
    let mut pending: Option<u8> = None;
    let flush = |bus: &mut B, dst: &mut u32, written: &mut u32, pending: &mut Option<u8>, byte: u8, vram: bool| {
        if vram {
            if let Some(low) = pending.take() {
                let v = (low as u16) | ((byte as u16) << 8);
                bus.write16(*dst, v);
                *dst = dst.wrapping_add(2);
            } else {
                *pending = Some(byte);
            }
        } else {
            bus.write8(*dst, byte);
            *dst = dst.wrapping_add(1);
        }
        *written += 1;
    };

    while written < total {
        let flags = bus.read8(src);
        src = src.wrapping_add(1);
        for bit in 0..8 {
            if written >= total { break; }
            if flags & (0x80 >> bit) == 0 {
                // Literal
                let b = bus.read8(src);
                src = src.wrapping_add(1);
                flush(bus, &mut dst, &mut written, &mut pending, b, vram);
            } else {
                // LZ reference
                let b0 = bus.read8(src);
                let b1 = bus.read8(src.wrapping_add(1));
                src = src.wrapping_add(2);
                let length = ((b0 >> 4) as u32) + 3;
                let disp = (((b0 & 0x0F) as u32) << 8) | (b1 as u32);
                // Read back from the *destination* — already-written bytes.
                // Distance is disp+1 from the next byte to write.
                for _ in 0..length {
                    if written >= total { break; }
                    let read_addr = dst.wrapping_sub(disp + 1).wrapping_sub(if vram { pending.is_some() as u32 } else { 0 });
                    let b = if vram && pending.is_some() {
                        // Pending half — synthesize from already-written halfword.
                        let halfword = bus.read16(read_addr & !1);
                        if read_addr & 1 != 0 { (halfword >> 8) as u8 } else { halfword as u8 }
                    } else {
                        bus.read8(read_addr)
                    };
                    flush(bus, &mut dst, &mut written, &mut pending, b, vram);
                }
            }
        }
    }

    // Flush a leftover half-byte for VRAM mode.
    if vram {
        if let Some(low) = pending {
            bus.write16(dst, low as u16);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::tests::TestBus;
    use crate::cpu::Cpu;

    #[test]
    fn test_div_basic() {
        let mut cpu = Cpu::new_arm9();
        cpu.regs[0] = 100;
        cpu.regs[1] = 7;
        swi_div(&mut cpu);
        assert_eq!(cpu.regs[0] as i32, 14);
        assert_eq!(cpu.regs[1] as i32, 2);
        assert_eq!(cpu.regs[3], 14);
    }

    #[test]
    fn test_div_negative() {
        let mut cpu = Cpu::new_arm9();
        cpu.regs[0] = (-100i32) as u32;
        cpu.regs[1] = 7;
        swi_div(&mut cpu);
        assert_eq!(cpu.regs[0] as i32, -14);
        assert_eq!(cpu.regs[3], 14);
    }

    #[test]
    fn test_div_by_zero_does_not_clobber() {
        let mut cpu = Cpu::new_arm9();
        cpu.regs[0] = 0xCAFE;
        cpu.regs[1] = 0;
        swi_div(&mut cpu);
        assert_eq!(cpu.regs[0], 0xCAFE);
    }

    #[test]
    fn test_div_arm_uses_reversed_inputs() {
        let mut cpu = Cpu::new_arm9();
        cpu.regs[0] = 7;
        cpu.regs[1] = 100;
        swi_div_arm(&mut cpu);
        assert_eq!(cpu.regs[0] as i32, 14);
        assert_eq!(cpu.regs[1] as i32, 2);
        assert_eq!(cpu.regs[3], 14);
    }

    #[test]
    fn test_sqrt() {
        let mut cpu = Cpu::new_arm9();
        cpu.regs[0] = 144;
        swi_sqrt(&mut cpu);
        assert_eq!(cpu.regs[0], 12);
        cpu.regs[0] = 1;
        swi_sqrt(&mut cpu);
        assert_eq!(cpu.regs[0], 1);
    }

    #[test]
    fn test_cpu_set_word_copy() {
        let mut cpu = Cpu::new_arm9();
        let mut bus = TestBus::new(0x1000);
        // Source data at 0x100
        for i in 0..16u32 {
            bus.mem[(0x100 + i * 4) as usize] = i as u8;
        }
        cpu.regs[0] = 0x100; // src
        cpu.regs[1] = 0x200; // dst
        cpu.regs[2] = 16 | (1 << 26); // 16 words, word transfer
        swi_cpu_set(&mut cpu, &mut bus);
        for i in 0..16 {
            assert_eq!(bus.mem[(0x100 + i * 4) as usize], bus.mem[(0x200 + i * 4) as usize]);
        }
    }

    #[test]
    fn test_cpu_set_halfword_fill() {
        let mut cpu = Cpu::new_arm9();
        let mut bus = TestBus::new(0x1000);
        // Fill source contains the value to repeat
        bus.mem[0x100] = 0x55;
        bus.mem[0x101] = 0xAA;
        cpu.regs[0] = 0x100;
        cpu.regs[1] = 0x200;
        cpu.regs[2] = 8 | (1 << 24); // 8 halfwords, fill
        swi_cpu_set(&mut cpu, &mut bus);
        for i in 0..8 {
            assert_eq!(bus.mem[(0x200 + i * 2) as usize], 0x55);
            assert_eq!(bus.mem[(0x200 + i * 2 + 1) as usize], 0xAA);
        }
    }

    #[test]
    fn test_cpu_fast_set_rounds_up_to_8() {
        let mut cpu = Cpu::new_arm9();
        let mut bus = TestBus::new(0x1000);
        for i in 0..8u32 {
            let bytes = (0xDEAD0000u32 | i).to_le_bytes();
            for b in 0..4 {
                bus.mem[(0x100 + i * 4 + b) as usize] = bytes[b as usize];
            }
        }
        cpu.regs[0] = 0x100;
        cpu.regs[1] = 0x200;
        cpu.regs[2] = 5; // 5 words → rounded to 8
        swi_cpu_fast_set(&mut cpu, &mut bus);
        // Verify all 8 words copied
        for i in 0..8 {
            for b in 0..4 {
                assert_eq!(
                    bus.mem[(0x100 + i * 4 + b) as usize],
                    bus.mem[(0x200 + i * 4 + b) as usize],
                );
            }
        }
    }

    #[test]
    fn test_lz77_uncomp_basic() {
        let mut cpu = Cpu::new_arm9();
        let mut bus = TestBus::new(0x1000);

        // Build a tiny LZ77 stream: 4 literal 'A's + a 4-byte backref to them.
        // Header: size=8, type=1
        let header: u32 = (8u32 << 8) | 0x10;
        let bytes = header.to_le_bytes();
        for b in 0..4 {
            bus.mem[0x100 + b] = bytes[b];
        }
        // Group 1: flags = 0b00000000 (8 literals)
        // We only need 4 literals for the first half, then a backref tag.
        // Simplest: flags = 0b0000_1000 → bits: 0,0,0,0,1,0,0,0
        // Wait, we need 4 lits + 1 ref, so: flags = 0b0000_1000? That marks bit 4 (5th from left = high bit?) — actually high bit is bit 7.
        // bit positions (high to low): 7,6,5,4,3,2,1,0.
        // Mask in code: (0x80 >> bit), so bit 0 of the loop tests flag bit 7.
        // Layout: 4 literals (flag bits 7,6,5,4 = 0), then a backref (flag bit 3 = 1). Remaining 3 bits unused.
        // flags = 0000_1000 binary = 0x08.
        bus.mem[0x104] = 0x08;
        bus.mem[0x105] = b'A';
        bus.mem[0x106] = b'A';
        bus.mem[0x107] = b'A';
        bus.mem[0x108] = b'A';
        // Backref: length-3=1 (=4 bytes), disp-1=0 (one byte before next)
        // byte0 = length<<4 | disp_high = (1 << 4) | 0 = 0x10
        // byte1 = disp_low = 0x00 → disp+1 = 1 (read one byte back from dst)
        bus.mem[0x109] = 0x10;
        bus.mem[0x10A] = 0x00;

        cpu.regs[0] = 0x100;
        cpu.regs[1] = 0x200;
        swi_lz77_uncomp(&mut cpu, &mut bus, false);

        for i in 0..8 {
            assert_eq!(bus.mem[0x200 + i], b'A',
                "byte {} should be 'A', got {}", i, bus.mem[0x200 + i]);
        }
    }
}

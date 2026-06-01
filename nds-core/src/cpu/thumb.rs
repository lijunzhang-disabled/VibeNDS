//! THUMB mode instruction decoder and executor.
//!
//! 19 ARMv4T formats plus the ARMv5T `BLX` register/immediate additions
//! gated on `is_arm9`.

use super::Cpu;
use super::alu::{ShiftType, add_with_carry, barrel_shift, sub_with_carry};
use super::bus::CpuBus;

impl Cpu {
    pub fn execute_thumb<B: CpuBus>(&mut self, bus: &mut B, opcode: u16) -> u32 {
        match opcode >> 8 {
            0x00..=0x07 => self.thumb_shift_imm(opcode),
            0x08..=0x0F => self.thumb_shift_imm(opcode),
            0x10..=0x17 => self.thumb_shift_imm(opcode),

            0x18..=0x19 => self.thumb_add_sub_reg(opcode),
            0x1A..=0x1B => self.thumb_add_sub_reg(opcode),
            0x1C..=0x1D => self.thumb_add_sub_imm(opcode),
            0x1E..=0x1F => self.thumb_add_sub_imm(opcode),

            0x20..=0x27 => self.thumb_mov_imm(opcode),
            0x28..=0x2F => self.thumb_cmp_imm(opcode),
            0x30..=0x37 => self.thumb_add_imm(opcode),
            0x38..=0x3F => self.thumb_sub_imm(opcode),

            0x40..=0x43 => self.thumb_alu(opcode),

            0x44..=0x47 => self.thumb_hi_reg_bx(opcode),

            0x48..=0x4F => self.thumb_ldr_pc(bus, opcode),

            0x50..=0x5F => self.thumb_load_store_reg(bus, opcode),

            0x60..=0x7F => self.thumb_load_store_imm(bus, opcode),

            0x80..=0x8F => self.thumb_load_store_half(bus, opcode),

            0x90..=0x9F => self.thumb_load_store_sp(bus, opcode),

            0xA0..=0xAF => self.thumb_load_address(opcode),

            0xB0 => self.thumb_add_sp(opcode),

            0xB4..=0xB5 => self.thumb_push(bus, opcode),
            0xBC..=0xBD => self.thumb_pop(bus, opcode),

            0xC0..=0xC7 => self.thumb_stmia(bus, opcode),
            0xC8..=0xCF => self.thumb_ldmia(bus, opcode),

            0xD0..=0xDD => self.thumb_cond_branch(opcode),

            0xDF => self.thumb_swi(opcode),

            0xE0..=0xE7 => self.thumb_branch(opcode),

            // BLX immediate suffix (ARMv5T): 1110_1xxx (0xE8..0xEF).
            // The prefix half is encoded as a normal BL prefix (0xF0..0xF7),
            // and the suffix at 0xE8..0xEF means "switch to ARM" instead of
            // staying in THUMB. Only valid on ARM9.
            0xE8..=0xEF => {
                if self.is_arm9 {
                    self.thumb_blx_suffix(opcode)
                } else {
                    log::warn!("THUMB undefined on ARM7: 0x{:04X}", opcode);
                    self.undefined_instruction();
                    1
                }
            }

            0xF0..=0xF7 => self.thumb_bl_prefix(opcode),
            0xF8..=0xFF => self.thumb_bl_suffix(opcode),

            _ => {
                log::warn!("THUMB undefined: 0x{:04X} at PC=0x{:08X}",
                    opcode, self.regs[15].wrapping_sub(4));
                1
            }
        }
    }

    fn thumb_shift_imm(&mut self, opcode: u16) -> u32 {
        let op = (opcode >> 11) & 3;
        let offset = ((opcode >> 6) & 0x1F) as u8;
        let rs = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let shift_type = match op {
            0 => ShiftType::Lsl,
            1 => ShiftType::Lsr,
            2 => ShiftType::Asr,
            _ => unreachable!(),
        };

        let (result, carry) = barrel_shift(self.reg(rs), shift_type, offset, self.cpsr.c(), true);
        self.regs[rd as usize] = result;
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        1
    }

    fn thumb_add_sub_reg(&mut self, opcode: u16) -> u32 {
        let sub = opcode & (1 << 9) != 0;
        let rn = ((opcode >> 6) & 7) as u8;
        let rs = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let a = self.reg(rs);
        let b = self.reg(rn);

        let (result, carry, overflow) = if sub {
            sub_with_carry(a, b, true)
        } else {
            add_with_carry(a, b, false)
        };

        self.regs[rd as usize] = result;
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        self.cpsr.set_v(overflow);
        1
    }

    fn thumb_add_sub_imm(&mut self, opcode: u16) -> u32 {
        let sub = opcode & (1 << 9) != 0;
        let imm = ((opcode >> 6) & 7) as u32;
        let rs = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let a = self.reg(rs);

        let (result, carry, overflow) = if sub {
            sub_with_carry(a, imm, true)
        } else {
            add_with_carry(a, imm, false)
        };

        self.regs[rd as usize] = result;
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        self.cpsr.set_v(overflow);
        1
    }

    fn thumb_mov_imm(&mut self, opcode: u16) -> u32 {
        let rd = ((opcode >> 8) & 7) as u8;
        let imm = (opcode & 0xFF) as u32;
        self.regs[rd as usize] = imm;
        self.cpsr.set_nz(imm);
        1
    }

    fn thumb_cmp_imm(&mut self, opcode: u16) -> u32 {
        let rd = ((opcode >> 8) & 7) as u8;
        let imm = (opcode & 0xFF) as u32;
        let (result, carry, overflow) = sub_with_carry(self.reg(rd), imm, true);
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        self.cpsr.set_v(overflow);
        1
    }

    fn thumb_add_imm(&mut self, opcode: u16) -> u32 {
        let rd = ((opcode >> 8) & 7) as u8;
        let imm = (opcode & 0xFF) as u32;
        let (result, carry, overflow) = add_with_carry(self.reg(rd), imm, false);
        self.regs[rd as usize] = result;
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        self.cpsr.set_v(overflow);
        1
    }

    fn thumb_sub_imm(&mut self, opcode: u16) -> u32 {
        let rd = ((opcode >> 8) & 7) as u8;
        let imm = (opcode & 0xFF) as u32;
        let (result, carry, overflow) = sub_with_carry(self.reg(rd), imm, true);
        self.regs[rd as usize] = result;
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        self.cpsr.set_v(overflow);
        1
    }

    fn thumb_alu(&mut self, opcode: u16) -> u32 {
        let op = (opcode >> 6) & 0xF;
        let rs = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let a = self.reg(rd);
        let b = self.reg(rs);

        match op {
            0x0 => { let r = a & b; self.regs[rd as usize] = r; self.cpsr.set_nz(r); }
            0x1 => { let r = a ^ b; self.regs[rd as usize] = r; self.cpsr.set_nz(r); }
            0x2 => {
                let (r, c) = barrel_shift(a, ShiftType::Lsl, b as u8, self.cpsr.c(), false);
                self.regs[rd as usize] = r; self.cpsr.set_nz(r); self.cpsr.set_c(c);
            }
            0x3 => {
                let (r, c) = barrel_shift(a, ShiftType::Lsr, b as u8, self.cpsr.c(), false);
                self.regs[rd as usize] = r; self.cpsr.set_nz(r); self.cpsr.set_c(c);
            }
            0x4 => {
                let (r, c) = barrel_shift(a, ShiftType::Asr, b as u8, self.cpsr.c(), false);
                self.regs[rd as usize] = r; self.cpsr.set_nz(r); self.cpsr.set_c(c);
            }
            0x5 => {
                let (r, c, v) = add_with_carry(a, b, self.cpsr.c());
                self.regs[rd as usize] = r;
                self.cpsr.set_nz(r); self.cpsr.set_c(c); self.cpsr.set_v(v);
            }
            0x6 => {
                let (r, c, v) = sub_with_carry(a, b, self.cpsr.c());
                self.regs[rd as usize] = r;
                self.cpsr.set_nz(r); self.cpsr.set_c(c); self.cpsr.set_v(v);
            }
            0x7 => {
                let (r, c) = barrel_shift(a, ShiftType::Ror, b as u8, self.cpsr.c(), false);
                self.regs[rd as usize] = r; self.cpsr.set_nz(r); self.cpsr.set_c(c);
            }
            0x8 => { let r = a & b; self.cpsr.set_nz(r); }
            0x9 => {
                let (r, c, v) = sub_with_carry(0, b, true);
                self.regs[rd as usize] = r;
                self.cpsr.set_nz(r); self.cpsr.set_c(c); self.cpsr.set_v(v);
            }
            0xA => {
                let (r, c, v) = sub_with_carry(a, b, true);
                self.cpsr.set_nz(r); self.cpsr.set_c(c); self.cpsr.set_v(v);
            }
            0xB => {
                let (r, c, v) = add_with_carry(a, b, false);
                self.cpsr.set_nz(r); self.cpsr.set_c(c); self.cpsr.set_v(v);
            }
            0xC => { let r = a | b; self.regs[rd as usize] = r; self.cpsr.set_nz(r); }
            0xD => { let r = a.wrapping_mul(b); self.regs[rd as usize] = r; self.cpsr.set_nz(r); }
            0xE => { let r = a & !b; self.regs[rd as usize] = r; self.cpsr.set_nz(r); }
            0xF => { let r = !b; self.regs[rd as usize] = r; self.cpsr.set_nz(r); }
            _ => unreachable!(),
        }
        1
    }

    fn thumb_hi_reg_bx(&mut self, opcode: u16) -> u32 {
        let op = (opcode >> 8) & 3;
        let h1 = (opcode >> 7) & 1;
        let h2 = (opcode >> 6) & 1;
        let rs = (((h2 << 3) | ((opcode >> 3) & 7)) & 0xF) as u8;
        let rd = (((h1 << 3) | (opcode & 7)) & 0xF) as u8;

        match op {
            0 => {
                let result = self.reg(rd).wrapping_add(self.reg(rs));
                if rd == 15 {
                    self.branch(result & !1);
                } else {
                    self.regs[rd as usize] = result;
                }
            }
            1 => {
                let (result, carry, overflow) = sub_with_carry(self.reg(rd), self.reg(rs), true);
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
                self.cpsr.set_v(overflow);
            }
            2 => {
                let val = self.reg(rs);
                if rd == 15 {
                    self.branch(val & !1);
                } else {
                    self.regs[rd as usize] = val;
                }
            }
            3 => {
                // Format 5 op=11: BX (h1=0) or BLX (h1=1, ARMv5T only)
                let addr = self.reg(rs);
                if h1 != 0 {
                    if self.is_arm9 {
                        // BLX Rm: LR = next instruction address | 1, then interwork
                        self.regs[14] = self.regs[15].wrapping_sub(2) | 1;
                        self.branch_exchange(addr);
                    } else {
                        log::warn!("THUMB BLX Rm on ARM7 (undefined)");
                        self.undefined_instruction();
                    }
                } else {
                    self.branch_exchange(addr);
                }
            }
            _ => unreachable!(),
        }

        if (op == 0 || op == 2) && rd == 15 { 3 } else if op == 3 { 3 } else { 1 }
    }

    fn thumb_ldr_pc<B: CpuBus>(&mut self, bus: &mut B, opcode: u16) -> u32 {
        let rd = ((opcode >> 8) & 7) as u8;
        let offset = ((opcode & 0xFF) as u32) << 2;
        let addr = (self.regs[15] & !3).wrapping_add(offset);
        let val = bus.read32(addr & !3);
        self.regs[rd as usize] = val;
        3
    }

    fn thumb_load_store_reg<B: CpuBus>(&mut self, bus: &mut B, opcode: u16) -> u32 {
        let ro = ((opcode >> 6) & 7) as u8;
        let rb = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let addr = self.reg(rb).wrapping_add(self.reg(ro));

        match (opcode >> 9) & 7 {
            0b000 => bus.write32(addr & !3, self.reg(rd)),
            0b001 => bus.write16(addr & !1, self.reg(rd) as u16),
            0b010 => bus.write8(addr, self.reg(rd) as u8),
            0b011 => self.regs[rd as usize] = bus.read8(addr) as i8 as i32 as u32,
            0b100 => {
                let val = bus.read32(addr & !3);
                let rotation = (addr & 3) * 8;
                self.regs[rd as usize] = val.rotate_right(rotation);
            }
            0b101 => {
                let val = bus.read16(addr & !1) as u32;
                self.regs[rd as usize] = if !self.is_arm9 && addr & 1 != 0 {
                    val.rotate_right(8)
                } else {
                    val
                };
            }
            0b110 => self.regs[rd as usize] = bus.read8(addr) as u32,
            0b111 => {
                if !self.is_arm9 && addr & 1 != 0 {
                    self.regs[rd as usize] = bus.read8(addr) as i8 as i32 as u32;
                } else {
                    self.regs[rd as usize] = bus.read16(addr & !1) as i16 as i32 as u32;
                }
            }
            _ => unreachable!(),
        }
        2
    }

    fn thumb_load_store_imm<B: CpuBus>(&mut self, bus: &mut B, opcode: u16) -> u32 {
        let b = opcode & (1 << 12) != 0;
        let l = opcode & (1 << 11) != 0;
        let offset = ((opcode >> 6) & 0x1F) as u32;
        let rb = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let base = self.reg(rb);
        let addr = if b { base.wrapping_add(offset) } else { base.wrapping_add(offset << 2) };

        if l {
            if b {
                self.regs[rd as usize] = bus.read8(addr) as u32;
            } else {
                let val = bus.read32(addr & !3);
                let rotation = (addr & 3) * 8;
                self.regs[rd as usize] = val.rotate_right(rotation);
            }
        } else if b {
            bus.write8(addr, self.reg(rd) as u8);
        } else {
            bus.write32(addr & !3, self.reg(rd));
        }
        2
    }

    fn thumb_load_store_half<B: CpuBus>(&mut self, bus: &mut B, opcode: u16) -> u32 {
        let l = opcode & (1 << 11) != 0;
        let offset = (((opcode >> 6) & 0x1F) as u32) << 1;
        let rb = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let addr = self.reg(rb).wrapping_add(offset);

        if l {
            let val = bus.read16(addr & !1) as u32;
            self.regs[rd as usize] = if !self.is_arm9 && addr & 1 != 0 {
                val.rotate_right(8)
            } else {
                val
            };
        } else {
            bus.write16(addr & !1, self.reg(rd) as u16);
        }
        2
    }

    fn thumb_load_store_sp<B: CpuBus>(&mut self, bus: &mut B, opcode: u16) -> u32 {
        let l = opcode & (1 << 11) != 0;
        let rd = ((opcode >> 8) & 7) as u8;
        let offset = ((opcode & 0xFF) as u32) << 2;
        let addr = self.regs[13].wrapping_add(offset);

        if l {
            let val = bus.read32(addr & !3);
            let rotation = (addr & 3) * 8;
            self.regs[rd as usize] = val.rotate_right(rotation);
        } else {
            bus.write32(addr & !3, self.reg(rd));
        }
        2
    }

    fn thumb_load_address(&mut self, opcode: u16) -> u32 {
        let sp = opcode & (1 << 11) != 0;
        let rd = ((opcode >> 8) & 7) as u8;
        let offset = ((opcode & 0xFF) as u32) << 2;

        if sp {
            self.regs[rd as usize] = self.regs[13].wrapping_add(offset);
        } else {
            let pc = self.regs[15] & !2;
            self.regs[rd as usize] = pc.wrapping_add(offset);
        }
        1
    }

    fn thumb_add_sp(&mut self, opcode: u16) -> u32 {
        let negative = opcode & (1 << 7) != 0;
        let offset = ((opcode & 0x7F) as u32) << 2;
        if negative {
            self.regs[13] = self.regs[13].wrapping_sub(offset);
        } else {
            self.regs[13] = self.regs[13].wrapping_add(offset);
        }
        1
    }

    fn thumb_push<B: CpuBus>(&mut self, bus: &mut B, opcode: u16) -> u32 {
        let lr = opcode & (1 << 8) != 0;
        let rlist = opcode & 0xFF;
        let reg_count = rlist.count_ones() + lr as u32;

        let mut addr = self.regs[13].wrapping_sub(reg_count * 4);
        self.regs[13] = addr;

        for i in 0..8u8 {
            if rlist & (1 << i) != 0 {
                bus.write32(addr, self.reg(i));
                addr = addr.wrapping_add(4);
            }
        }
        if lr {
            bus.write32(addr, self.regs[14]);
        }
        reg_count + 1
    }

    fn thumb_pop<B: CpuBus>(&mut self, bus: &mut B, opcode: u16) -> u32 {
        let pc = opcode & (1 << 8) != 0;
        let rlist = opcode & 0xFF;

        let mut addr = self.regs[13];

        for i in 0..8u8 {
            if rlist & (1 << i) != 0 {
                self.regs[i as usize] = bus.read32(addr);
                addr = addr.wrapping_add(4);
            }
        }
        if pc {
            let val = bus.read32(addr);
            addr = addr.wrapping_add(4);
            // ARMv5T: POP {..., PC} interworks via bit 0. ARMv4T: stays in THUMB.
            if self.is_arm9 {
                self.branch_exchange(val);
            } else {
                self.branch(val & !1);
            }
        }

        self.regs[13] = addr;

        let reg_count = rlist.count_ones() + pc as u32;
        reg_count + 2
    }

    fn thumb_stmia<B: CpuBus>(&mut self, bus: &mut B, opcode: u16) -> u32 {
        let rb = ((opcode >> 8) & 7) as u8;
        let rlist = opcode & 0xFF;
        let base = self.reg(rb);
        let mut addr = base;

        if rlist == 0 {
            bus.write32(addr, self.reg(15).wrapping_add(2));
            self.regs[rb as usize] = base.wrapping_add(0x40);
            return 2;
        }

        let rb_in_list = rlist & (1 << rb) != 0;
        let rb_is_lowest = rb_in_list && (rlist & ((1 << rb) - 1)) == 0;
        let final_addr = base.wrapping_add(rlist.count_ones() * 4);

        for i in 0..8u8 {
            if rlist & (1 << i) != 0 {
                let val = if i == rb && rb_in_list && !rb_is_lowest { final_addr } else { self.reg(i) };
                bus.write32(addr, val);
                addr = addr.wrapping_add(4);
            }
        }

        self.regs[rb as usize] = addr;
        rlist.count_ones() + 1
    }

    fn thumb_ldmia<B: CpuBus>(&mut self, bus: &mut B, opcode: u16) -> u32 {
        let rb = ((opcode >> 8) & 7) as u8;
        let rlist = opcode & 0xFF;
        let base = self.reg(rb);
        let mut addr = base;

        if rlist == 0 {
            let val = bus.read32(addr);
            self.regs[rb as usize] = base.wrapping_add(0x40);
            self.branch(val & !1);
            return 5;
        }

        for i in 0..8u8 {
            if rlist & (1 << i) != 0 {
                self.regs[i as usize] = bus.read32(addr);
                addr = addr.wrapping_add(4);
            }
        }

        if rlist & (1 << rb) == 0 {
            self.regs[rb as usize] = addr;
        }

        rlist.count_ones() + 2
    }

    fn thumb_cond_branch(&mut self, opcode: u16) -> u32 {
        let cond = (opcode >> 8) & 0xF;
        if !self.check_condition(cond as u32) {
            return 1;
        }
        let offset = ((opcode & 0xFF) as i8 as i32) << 1;
        let target = (self.regs[15] as i32).wrapping_add(offset) as u32;
        self.branch(target);
        3
    }

    fn thumb_swi(&mut self, opcode: u16) -> u32 {
        let comment = (opcode & 0xFF) as u8;
        self.pending_swi = Some(comment);
        3
    }

    fn thumb_branch(&mut self, opcode: u16) -> u32 {
        let offset = (((opcode & 0x7FF) as i32) << 21) >> 20;
        let target = (self.regs[15] as i32).wrapping_add(offset) as u32;
        self.branch(target);
        3
    }

    fn thumb_bl_prefix(&mut self, opcode: u16) -> u32 {
        let offset = (((opcode & 0x7FF) as i32) << 21) >> 9;
        self.regs[14] = (self.regs[15] as i32).wrapping_add(offset) as u32;
        1
    }

    fn thumb_bl_suffix(&mut self, opcode: u16) -> u32 {
        let offset = ((opcode & 0x7FF) as u32) << 1;
        let next_instr = self.regs[15].wrapping_sub(2);
        let target = self.regs[14].wrapping_add(offset);
        self.regs[14] = next_instr | 1;
        self.branch(target);
        4
    }

    /// BLX immediate suffix (ARMv5T): the prefix is the same as a BL prefix
    /// (0xF0..0xF7), but this suffix at 0xE8..0xEF means "call ARM target".
    fn thumb_blx_suffix(&mut self, opcode: u16) -> u32 {
        // bit 0 of the offset is forced to 0 (target must be word-aligned for ARM)
        let offset = ((opcode & 0x7FF) as u32) << 1;
        let next_instr = self.regs[15].wrapping_sub(2);
        let target = self.regs[14].wrapping_add(offset) & !3;
        self.regs[14] = next_instr | 1;
        self.cpsr.set_thumb(false);
        self.branch(target);
        4
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestBus;
    use super::super::CpuMode;
    use super::*;

    fn arm7_thumb(mem: usize) -> (Cpu, TestBus) {
        let mut cpu = Cpu::new_arm7();
        cpu.cpsr = super::super::Psr::new(CpuMode::System);
        cpu.cpsr.bits &= !(1 << 7);
        cpu.cpsr.set_thumb(true);
        let bus = TestBus::new(mem);
        (cpu, bus)
    }

    fn arm9_thumb(mem: usize) -> (Cpu, TestBus) {
        let mut cpu = Cpu::new_arm9();
        cpu.cpsr = super::super::Psr::new(CpuMode::System);
        cpu.cpsr.bits &= !(1 << 7);
        cpu.cpsr.set_thumb(true);
        let bus = TestBus::new(mem);
        (cpu, bus)
    }

    #[test]
    fn test_thumb_mov_imm() {
        let (mut cpu, mut bus) = arm7_thumb(0x100);
        cpu.execute_thumb(&mut bus, 0x202A);
        assert_eq!(cpu.regs[0], 42);
    }

    #[test]
    fn test_thumb_add_imm() {
        let (mut cpu, mut bus) = arm7_thumb(0x100);
        cpu.regs[0] = 10;
        cpu.execute_thumb(&mut bus, 0x3005);
        assert_eq!(cpu.regs[0], 15);
    }

    #[test]
    fn test_thumb_push_pop() {
        let (mut cpu, mut bus) = arm7_thumb(0x200);
        cpu.regs[0] = 0xAAAA;
        cpu.regs[1] = 0xBBBB;
        cpu.regs[13] = 0x100;

        cpu.execute_thumb(&mut bus, 0xB403); // PUSH {R0, R1}
        assert_eq!(cpu.regs[13], 0xF8);

        cpu.regs[0] = 0;
        cpu.regs[1] = 0;
        cpu.execute_thumb(&mut bus, 0xBC03); // POP {R0, R1}
        assert_eq!(cpu.regs[0], 0xAAAA);
        assert_eq!(cpu.regs[1], 0xBBBB);
        assert_eq!(cpu.regs[13], 0x100);
    }

    #[test]
    fn test_thumb_blx_register_arm9() {
        let (mut cpu, mut bus) = arm9_thumb(0x100);
        cpu.regs[15] = 0x100; // PC = exec+4
        cpu.regs[2] = 0x40;   // ARM target (bit 0 = 0)
        // Format 5 op=11 with h1=1: BLX Rm. Encoding bits:
        //   [15:8] = 0100_0111 (0x47), [7:6] = 0b11 (op=3), bit 7 = h1 = 1
        // Actually the format is:
        //   [15:10] = 010001
        //   [9:8]   = op
        //   [7]     = H1
        //   [6]     = H2
        //   [5:3]   = Rs (low)
        //   [2:0]   = Rd (low)
        // BLX Rm: op=11, H1=1, so bits [9:7] = 111, [15:10] = 010001
        // Top byte = 0100_0111 = 0x47, low byte = 1_H2_Rs(3)_000 with H2=0, Rs=2 → 1_0_010_000 = 0x90
        // Full opcode = 0x4790
        cpu.execute_thumb(&mut bus, 0x4790);
        assert_eq!(cpu.regs[14], (0x100 - 2) | 1); // LR = next | 1
        assert_eq!(cpu.regs[15], 0x40);
        assert!(!cpu.cpsr.thumb()); // switched to ARM (bit 0 of target = 0)
    }

    #[test]
    fn test_thumb_pop_pc_interworks_on_arm9() {
        let (mut cpu, mut bus) = arm9_thumb(0x200);
        cpu.regs[13] = 0x100;
        // Place an ARM-mode address (bit 0 = 0) at SP
        bus.mem[0x100..0x104].copy_from_slice(&0x40u32.to_le_bytes());
        // POP {PC}: rlist = 0, P = 1. Opcode = 1011_1100_P_rlist = 0xBD00
        cpu.execute_thumb(&mut bus, 0xBD00);
        assert_eq!(cpu.regs[15], 0x40);
        assert!(!cpu.cpsr.thumb()); // ARM9 interworks → ARM mode
    }

    #[test]
    fn test_thumb_pop_pc_stays_thumb_on_arm7() {
        let (mut cpu, mut bus) = arm7_thumb(0x200);
        cpu.regs[13] = 0x100;
        bus.mem[0x100..0x104].copy_from_slice(&0x40u32.to_le_bytes());
        cpu.execute_thumb(&mut bus, 0xBD00);
        assert_eq!(cpu.regs[15], 0x40);
        assert!(cpu.cpsr.thumb()); // ARM7 stays in THUMB
    }

    #[test]
    fn test_thumb_arm9_unaligned_ldr_rotates_aligned_word() {
        let (mut cpu, mut bus) = arm9_thumb(0x100);
        bus.mem[0x40..0x44].copy_from_slice(&0xFF00_8F00u32.to_le_bytes());
        cpu.regs[0] = 0x42;

        cpu.execute_thumb(&mut bus, 0x6801); // LDR R1, [R0, #0]

        assert_eq!(cpu.regs[1], 0x8F00_FF00);
    }

    #[test]
    fn test_thumb_arm9_unaligned_register_ldr_rotates_aligned_word() {
        let (mut cpu, mut bus) = arm9_thumb(0x100);
        bus.mem[0x40..0x44].copy_from_slice(&0xFF00_8F00u32.to_le_bytes());
        cpu.regs[0] = 0x40;
        cpu.regs[2] = 2;

        cpu.execute_thumb(&mut bus, 0x5881); // LDR R1, [R0, R2]

        assert_eq!(cpu.regs[1], 0x8F00_FF00);
    }
}

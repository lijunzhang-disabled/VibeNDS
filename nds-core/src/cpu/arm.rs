//! ARM mode instruction decoder and executor.
//!
//! Covers the ARMv4T baseline (shared with ARM7TDMI) plus the ARMv5TE
//! additions used by the ARM946E-S: CLZ, BLX (immediate + register), QADD /
//! QSUB / QDADD / QDSUB, the SMLA*/SMLAW*/SMLAL*/SMUL*/SMULW* DSP-multiply
//! family, LDRD/STRD, MCR/MRC/CDP, and PLD.
//!
//! ARMv5TE encodings only execute when `cpu.is_arm9` is true. On the ARM7
//! they fall through to the undefined-instruction handler.

use super::Cpu;
use super::alu::{
    AluOp, ShiftType,
    add_with_carry, barrel_shift, sub_with_carry,
    signed_sat_add, signed_sat_sub, signed_sat_double,
};
use super::bus::CpuBus;
use super::CpuMode;

impl Cpu {
    /// Execute one ARM instruction. Returns cycles consumed.
    pub fn execute_arm<B: CpuBus>(&mut self, bus: &mut B, opcode: u32) -> u32 {
        // ARMv5+ unconditional encoding space (cond = 0xF) is decoded
        // separately. The ARM7 treats it as undefined.
        if opcode >> 28 == 0xF {
            if self.is_arm9 {
                return self.execute_arm_unconditional(bus, opcode);
            } else {
                return self.arm_undefined(opcode);
            }
        }

        let bits_27_20 = (opcode >> 20) & 0xFF;
        let bits_7_4 = (opcode >> 4) & 0xF;

        match bits_27_20 >> 5 {
            0b000 => {
                // Family: data processing / multiply / SWP / halfword / BX /
                // ARMv5TE extras (CLZ, BLX Rm, Q*, SMLA*, etc).
                if bits_27_20 == 0x12 && bits_7_4 == 0x1 {
                    self.arm_branch_exchange(opcode)
                } else if bits_27_20 == 0x12 && bits_7_4 == 0x3 {
                    // BLX register (ARMv5TE)
                    if self.is_arm9 { self.arm_branch_link_exchange_reg(opcode) }
                    else { self.arm_undefined(opcode) }
                } else if bits_27_20 == 0x16 && bits_7_4 == 0x1 {
                    // CLZ (ARMv5TE)
                    if self.is_arm9 { self.arm_clz(opcode) }
                    else { self.arm_undefined(opcode) }
                } else if bits_7_4 == 0x5
                    && (bits_27_20 == 0x10 || bits_27_20 == 0x12
                        || bits_27_20 == 0x14 || bits_27_20 == 0x16)
                {
                    // QADD / QSUB / QDADD / QDSUB (ARMv5TE)
                    if self.is_arm9 { self.arm_q_arith(opcode) }
                    else { self.arm_undefined(opcode) }
                } else if (bits_7_4 & 0x9) == 0x8 && (bits_7_4 & 0x1) == 0x0
                    && (bits_27_20 & 0xE0) == 0
                    && bits_27_20 & 0x10 != 0
                {
                    // DSP multiply family: bit 7 = 1, bit 4 = 0, bits[27:24] = 0x1
                    if self.is_arm9 { self.arm_dsp_multiply(opcode) }
                    else { self.arm_undefined(opcode) }
                } else if (bits_7_4 & 0x9) == 0x9 && (bits_27_20 & 0xE0) == 0 {
                    // Multiply / multiply-long / SWP / halfword / LDRD/STRD
                    match bits_7_4 {
                        0x9 => {
                            if bits_27_20 & 0xFC == 0x00 {
                                self.arm_multiply(opcode)
                            } else if bits_27_20 & 0xF8 == 0x08 {
                                self.arm_multiply_long(opcode)
                            } else if bits_27_20 & 0xFB == 0x10 {
                                self.arm_swap(bus, opcode)
                            } else {
                                self.arm_undefined(opcode)
                            }
                        }
                        0xB => self.arm_halfword_transfer(bus, opcode),
                        0xD | 0xF => {
                            // ARMv4T: signed halfword load (LDRSB=0xD, LDRSH=0xF
                            // when L=1). LDRD/STRD on ARMv5TE collides — we
                            // disambiguate by L bit + is_arm9.
                            let l = opcode & (1 << 20) != 0;
                            if !l && self.is_arm9 {
                                // STRD (D=0xF) or LDRD (D=0xD with L=0): both
                                // are the doubleword-transfer encoding when L=0.
                                self.arm_doubleword_transfer(bus, opcode)
                            } else {
                                self.arm_halfword_transfer(bus, opcode)
                            }
                        }
                        _ => self.arm_data_processing(opcode),
                    }
                } else {
                    // PSR transfers (MRS / MSR) and data-processing register form
                    if (bits_27_20 & 0xFB) == 0x10 && bits_7_4 == 0x0 {
                        self.arm_mrs(opcode)
                    } else if (bits_27_20 & 0xFB) == 0x12 && bits_7_4 == 0x0 {
                        self.arm_msr(opcode)
                    } else {
                        self.arm_data_processing(opcode)
                    }
                }
            }
            0b001 => {
                if (bits_27_20 & 0xFB) == 0x32 {
                    self.arm_msr(opcode)
                } else {
                    self.arm_data_processing(opcode)
                }
            }
            0b010 => self.arm_single_transfer(bus, opcode),
            0b011 => {
                if opcode & (1 << 4) != 0 {
                    self.arm_undefined(opcode)
                } else {
                    self.arm_single_transfer(bus, opcode)
                }
            }
            0b100 => self.arm_block_transfer(bus, opcode),
            0b101 => self.arm_branch(opcode),
            0b110 => {
                // Coprocessor LDC/STC (ARMv5+). NDS doesn't use them; NOP.
                if self.is_arm9 { 1 } else { self.arm_undefined(opcode) }
            }
            0b111 => {
                if (opcode >> 24) & 0xF == 0xF {
                    self.arm_swi(opcode)
                } else if self.is_arm9 {
                    // Coprocessor MCR/MRC/CDP. Only CP15 exists on the NDS.
                    self.arm_coprocessor(opcode)
                } else {
                    self.arm_undefined(opcode)
                }
            }
            _ => self.arm_undefined(opcode),
        }
    }

    /// Decode the ARMv5+ unconditional encoding space (cond = 0xF).
    fn execute_arm_unconditional<B: CpuBus>(&mut self, _bus: &mut B, opcode: u32) -> u32 {
        let bits_27_25 = (opcode >> 25) & 0b111;
        match bits_27_25 {
            0b101 => {
                // BLX immediate: 1111 101H imm24
                let h = (opcode >> 24) & 1;
                // 24-bit signed offset, sign-extended and shifted left 2.
                // Add `H << 1` for halfword granularity.
                let mut offset = ((opcode & 0x00FF_FFFF) as i32) << 8 >> 6;
                offset |= (h as i32) << 1;
                let target = (self.regs[15] as i32).wrapping_add(offset) as u32;

                self.regs[14] = self.regs[15].wrapping_sub(4); // LR = next instruction
                // Switch to THUMB regardless of target bit 0 (BLX always interworks).
                self.cpsr.set_thumb(true);
                self.regs[15] = target;
                self.pipeline_flushed = true;
                3
            }
            0b010 | 0b011 => {
                // PLD (preload data) — hint, NOP for us.
                1
            }
            _ => {
                log::trace!("ARMv5 unconditional unhandled: 0x{:08X}", opcode);
                1
            }
        }
    }

    // ─── Data Processing ──────────────────────────────────────────

    fn arm_data_processing(&mut self, opcode: u32) -> u32 {
        let i = opcode & (1 << 25) != 0;
        let s = opcode & (1 << 20) != 0;
        let op = AluOp::from_u8(((opcode >> 21) & 0xF) as u8);
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rd = ((opcode >> 12) & 0xF) as u8;

        let shift_by_reg = !i && opcode & (1 << 4) != 0;

        let op1 = if rn == 15 && shift_by_reg {
            self.reg(15).wrapping_add(4)
        } else {
            self.reg(rn)
        };

        let (op2, shifter_carry) = if i {
            let imm = opcode & 0xFF;
            let rotate = ((opcode >> 8) & 0xF) * 2;
            if rotate == 0 {
                (imm, self.cpsr.c())
            } else {
                let result = imm.rotate_right(rotate);
                (result, result >> 31 != 0)
            }
        } else {
            let rm = (opcode & 0xF) as u8;
            let shift_type = ShiftType::from_u8(((opcode >> 5) & 3) as u8);

            let shift_amount = if shift_by_reg {
                let rs = ((opcode >> 8) & 0xF) as u8;
                let rs_val = if rs == 15 { self.reg(15).wrapping_add(4) } else { self.reg(rs) };
                rs_val as u8
            } else {
                ((opcode >> 7) & 0x1F) as u8
            };

            let rm_val = if rm == 15 && shift_by_reg {
                self.reg(15).wrapping_add(4)
            } else {
                self.reg(rm)
            };

            let immediate_shift = !shift_by_reg;
            barrel_shift(rm_val, shift_type, shift_amount, self.cpsr.c(), immediate_shift)
        };

        let (result, carry, overflow) = match op {
            AluOp::And | AluOp::Tst => (op1 & op2, shifter_carry, self.cpsr.v()),
            AluOp::Eor | AluOp::Teq => (op1 ^ op2, shifter_carry, self.cpsr.v()),
            AluOp::Sub | AluOp::Cmp => sub_with_carry(op1, op2, true),
            AluOp::Rsb => sub_with_carry(op2, op1, true),
            AluOp::Add | AluOp::Cmn => add_with_carry(op1, op2, false),
            AluOp::Adc => add_with_carry(op1, op2, self.cpsr.c()),
            AluOp::Sbc => sub_with_carry(op1, op2, self.cpsr.c()),
            AluOp::Rsc => sub_with_carry(op2, op1, self.cpsr.c()),
            AluOp::Orr => (op1 | op2, shifter_carry, self.cpsr.v()),
            AluOp::Mov => (op2, shifter_carry, self.cpsr.v()),
            AluOp::Bic => (op1 & !op2, shifter_carry, self.cpsr.v()),
            AluOp::Mvn => (!op2, shifter_carry, self.cpsr.v()),
        };

        if s {
            if rd == 15 {
                if op.is_test() {
                    let spsr = self.spsr();
                    let new_mode = spsr.mode();
                    self.switch_mode(new_mode);
                    self.cpsr = spsr;
                } else {
                    self.set_reg_with_flags(rd, result, true);
                }
            } else {
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
                if !op.is_logical() {
                    self.cpsr.set_v(overflow);
                }
                if !op.is_test() {
                    self.regs[rd as usize] = result;
                }
            }
        } else if !op.is_test() {
            self.set_reg(rd, result);
        }

        1
    }

    // ─── Multiply (ARMv4) ─────────────────────────────────────────

    fn arm_multiply(&mut self, opcode: u32) -> u32 {
        let a = opcode & (1 << 21) != 0;
        let s = opcode & (1 << 20) != 0;
        let rd = ((opcode >> 16) & 0xF) as u8;
        let rn = ((opcode >> 12) & 0xF) as u8;
        let rs = ((opcode >> 8) & 0xF) as u8;
        let rm = (opcode & 0xF) as u8;

        let result = if a {
            self.reg(rm).wrapping_mul(self.reg(rs)).wrapping_add(self.reg(rn))
        } else {
            self.reg(rm).wrapping_mul(self.reg(rs))
        };

        self.regs[rd as usize] = result;

        if s {
            self.cpsr.set_nz(result);
        }
        4
    }

    fn arm_multiply_long(&mut self, opcode: u32) -> u32 {
        let u = opcode & (1 << 22) != 0;
        let a = opcode & (1 << 21) != 0;
        let s = opcode & (1 << 20) != 0;
        let rd_hi = ((opcode >> 16) & 0xF) as u8;
        let rd_lo = ((opcode >> 12) & 0xF) as u8;
        let rs = ((opcode >> 8) & 0xF) as u8;
        let rm = (opcode & 0xF) as u8;

        let result = if u {
            let result = (self.reg(rm) as i32 as i64) * (self.reg(rs) as i32 as i64);
            if a {
                let acc = ((self.reg(rd_hi) as u64) << 32) | self.reg(rd_lo) as u64;
                (result as u64).wrapping_add(acc)
            } else {
                result as u64
            }
        } else {
            let result = (self.reg(rm) as u64) * (self.reg(rs) as u64);
            if a {
                let acc = ((self.reg(rd_hi) as u64) << 32) | self.reg(rd_lo) as u64;
                result.wrapping_add(acc)
            } else {
                result
            }
        };

        self.regs[rd_lo as usize] = result as u32;
        self.regs[rd_hi as usize] = (result >> 32) as u32;

        if s {
            self.cpsr.set_n((result >> 63) != 0);
            self.cpsr.set_z(result == 0);
        }

        5
    }

    // ─── Single Data Transfer (LDR/STR) ──────────────────────────

    fn arm_single_transfer<B: CpuBus>(&mut self, bus: &mut B, opcode: u32) -> u32 {
        let i = opcode & (1 << 25) != 0;
        let p = opcode & (1 << 24) != 0;
        let u = opcode & (1 << 23) != 0;
        let b = opcode & (1 << 22) != 0;
        let w = opcode & (1 << 21) != 0;
        let l = opcode & (1 << 20) != 0;
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rd = ((opcode >> 12) & 0xF) as u8;

        let base = self.reg(rn);

        let offset = if !i {
            opcode & 0xFFF
        } else {
            let rm = (opcode & 0xF) as u8;
            let shift_type = ShiftType::from_u8(((opcode >> 5) & 3) as u8);
            let shift_amount = ((opcode >> 7) & 0x1F) as u8;
            let (shifted, _) = barrel_shift(self.reg(rm), shift_type, shift_amount, self.cpsr.c(), true);
            shifted
        };

        let offset_addr = if u { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let addr = if p { offset_addr } else { base };

        let mut cycles = 1;

        if l {
            let val = if b {
                bus.read8(addr) as u32
            } else if self.is_arm9 {
                // ARMv5: misaligned LDR raises a data abort or returns the
                // word at the aligned address (no rotate). We follow the
                // "force-aligned" behavior melonDS uses.
                bus.read32(addr & !3)
            } else {
                let aligned = addr & !3;
                let val = bus.read32(aligned);
                let rotation = (addr & 3) * 8;
                val.rotate_right(rotation)
            };
            self.set_reg(rd, val);
            cycles += 1;
        } else {
            let val = if rd == 15 { self.reg(15).wrapping_add(4) } else { self.reg(rd) };
            if b {
                bus.write8(addr, val as u8);
            } else {
                bus.write32(addr & !3, val);
            }
        }

        if (!p || w) && !(l && rn == rd) {
            if rn != 15 {
                self.regs[rn as usize] = offset_addr;
            }
        }

        cycles
    }

    // ─── Halfword / Signed Data Transfer ─────────────────────────

    fn arm_halfword_transfer<B: CpuBus>(&mut self, bus: &mut B, opcode: u32) -> u32 {
        let p = opcode & (1 << 24) != 0;
        let u = opcode & (1 << 23) != 0;
        let i = opcode & (1 << 22) != 0;
        let w = opcode & (1 << 21) != 0;
        let l = opcode & (1 << 20) != 0;
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rd = ((opcode >> 12) & 0xF) as u8;
        let sh = (opcode >> 5) & 3;

        let base = self.reg(rn);

        let offset = if i {
            ((opcode >> 4) & 0xF0) | (opcode & 0xF)
        } else {
            let rm = (opcode & 0xF) as u8;
            self.reg(rm)
        };

        let offset_addr = if u { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let addr = if p { offset_addr } else { base };

        if l {
            let val = match sh {
                0x1 => {
                    let val = bus.read16(addr & !1) as u32;
                    if !self.is_arm9 && addr & 1 != 0 { val.rotate_right(8) } else { val }
                }
                0x2 => bus.read8(addr) as i8 as i32 as u32,
                0x3 => {
                    if !self.is_arm9 && addr & 1 != 0 {
                        bus.read8(addr) as i8 as i32 as u32
                    } else {
                        bus.read16(addr & !1) as i16 as i32 as u32
                    }
                }
                _ => 0,
            };
            self.set_reg(rd, val);
        } else {
            let val = self.reg(rd);
            bus.write16(addr & !1, val as u16);
        }

        if (!p || w) && !(l && rn == rd) {
            if rn != 15 {
                self.regs[rn as usize] = offset_addr;
            }
        }

        if l { 3 } else { 2 }
    }

    // ─── Doubleword Transfer (LDRD/STRD, ARMv5TE) ────────────────

    fn arm_doubleword_transfer<B: CpuBus>(&mut self, bus: &mut B, opcode: u32) -> u32 {
        let p = opcode & (1 << 24) != 0;
        let u = opcode & (1 << 23) != 0;
        let i = opcode & (1 << 22) != 0;
        let w = opcode & (1 << 21) != 0;
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rd = ((opcode >> 12) & 0xF) as u8;
        // S/H field: 0xD = LDRD (load), 0xF = STRD (store)
        let store = (opcode >> 4) & 0xF == 0xF;

        if rd & 1 != 0 {
            // Rd must be even — UNPREDICTABLE if odd. Treat as undefined.
            return self.arm_undefined(opcode);
        }

        let base = self.reg(rn);
        let offset = if i {
            ((opcode >> 4) & 0xF0) | (opcode & 0xF)
        } else {
            let rm = (opcode & 0xF) as u8;
            self.reg(rm)
        };
        let offset_addr = if u { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let addr = if p { offset_addr } else { base };
        let aligned = addr & !7;

        if store {
            bus.write32(aligned, self.reg(rd));
            bus.write32(aligned.wrapping_add(4), self.reg(rd + 1));
        } else {
            self.regs[rd as usize] = bus.read32(aligned);
            self.regs[(rd + 1) as usize] = bus.read32(aligned.wrapping_add(4));
        }

        if (!p || w) && rn != 15 {
            self.regs[rn as usize] = offset_addr;
        }

        if store { 3 } else { 4 }
    }

    // ─── Block Data Transfer (LDM/STM) ───────────────────────────

    fn arm_block_transfer<B: CpuBus>(&mut self, bus: &mut B, opcode: u32) -> u32 {
        let p = opcode & (1 << 24) != 0;
        let u = opcode & (1 << 23) != 0;
        let s = opcode & (1 << 22) != 0;
        let w = opcode & (1 << 21) != 0;
        let l = opcode & (1 << 20) != 0;
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rlist = (opcode & 0xFFFF) as u16;

        let base = self.reg(rn);
        let reg_count = rlist.count_ones();

        if rlist == 0 {
            // Empty rlist: ARMv4T quirk — transfer R15, writeback ±0x40.
            // ARMv5 makes this UNPREDICTABLE; we keep the v4 behavior since
            // it never appears in real ARM9 code anyway.
            let xfer_addr = match (u, p) {
                (true,  false) => base,
                (true,  true)  => base.wrapping_add(4),
                (false, false) => base.wrapping_sub(0x3C),
                (false, true)  => base.wrapping_sub(0x40),
            };
            if l {
                let val = bus.read32(xfer_addr);
                self.branch(val & !1);
            } else {
                bus.write32(xfer_addr, self.reg(15).wrapping_add(4));
            }
            if w {
                self.regs[rn as usize] = if u {
                    base.wrapping_add(0x40)
                } else {
                    base.wrapping_sub(0x40)
                };
            }
            return 3;
        }

        let mut addr = if u {
            if p { base.wrapping_add(4) } else { base }
        } else {
            let total = reg_count * 4;
            if p { base.wrapping_sub(total) } else { base.wrapping_sub(total).wrapping_add(4) }
        };

        let final_addr = if u {
            base.wrapping_add(reg_count * 4)
        } else {
            base.wrapping_sub(reg_count * 4)
        };

        let r15_in_list = rlist & (1 << 15) != 0;
        let use_user_bank = s && !(l && r15_in_list);

        let rn_in_list = rlist & (1 << rn) != 0;
        let rn_is_lowest = rn_in_list && rlist.trailing_zeros() as u8 == rn;

        for i in 0..16u8 {
            if rlist & (1 << i) == 0 {
                continue;
            }

            if l {
                let val = bus.read32(addr & !3);
                if s && r15_in_list {
                    if i == 15 {
                        let spsr = self.spsr();
                        let new_mode = spsr.mode();
                        self.switch_mode(new_mode);
                        self.cpsr = spsr;
                        // ARMv5 interworks; ARMv4 ignores bit 0.
                        if self.is_arm9 {
                            self.branch_exchange(val);
                        } else {
                            self.branch(val & !1);
                        }
                    } else {
                        self.regs[i as usize] = val;
                    }
                } else if i == 15 {
                    if self.is_arm9 {
                        self.branch_exchange(val);
                    } else {
                        self.branch(val & !1);
                    }
                } else if use_user_bank {
                    self.write_user_reg(i, val);
                } else {
                    self.regs[i as usize] = val;
                }
            } else {
                let val = if i == 15 {
                    self.reg(15).wrapping_add(4)
                } else if i == rn && rn_in_list && !rn_is_lowest && w {
                    final_addr
                } else if use_user_bank {
                    self.read_user_reg(i)
                } else {
                    self.reg(i)
                };
                bus.write32(addr & !3, val);
            }

            addr = addr.wrapping_add(4);
        }

        if w && !(l && rlist & (1 << rn) != 0) {
            self.regs[rn as usize] = final_addr;
        }

        if l { reg_count + 2 } else { reg_count + 1 }
    }

    // ─── Branch / Branch with Link ───────────────────────────────

    fn arm_branch(&mut self, opcode: u32) -> u32 {
        let link = opcode & (1 << 24) != 0;
        let offset = ((opcode & 0x00FF_FFFF) as i32) << 8 >> 6;

        if link {
            self.regs[14] = self.regs[15].wrapping_sub(4);
        }

        let target = (self.regs[15] as i32).wrapping_add(offset) as u32;
        self.branch(target);
        3
    }

    // ─── Branch and Exchange (BX) ────────────────────────────────

    fn arm_branch_exchange(&mut self, opcode: u32) -> u32 {
        let rm = (opcode & 0xF) as u8;
        let addr = self.reg(rm);
        self.branch_exchange(addr);
        3
    }

    // ─── BLX register (ARMv5TE) ──────────────────────────────────

    fn arm_branch_link_exchange_reg(&mut self, opcode: u32) -> u32 {
        let rm = (opcode & 0xF) as u8;
        let addr = self.reg(rm);
        self.regs[14] = self.regs[15].wrapping_sub(4); // LR = next instruction
        self.branch_exchange(addr);
        3
    }

    // ─── CLZ (ARMv5TE) ───────────────────────────────────────────

    fn arm_clz(&mut self, opcode: u32) -> u32 {
        let rd = ((opcode >> 12) & 0xF) as u8;
        let rm = (opcode & 0xF) as u8;
        let val = self.reg(rm);
        let result = if val == 0 { 32 } else { val.leading_zeros() };
        self.regs[rd as usize] = result;
        1
    }

    // ─── QADD / QSUB / QDADD / QDSUB (ARMv5TE) ───────────────────

    fn arm_q_arith(&mut self, opcode: u32) -> u32 {
        let kind = (opcode >> 21) & 0x3;
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rd = ((opcode >> 12) & 0xF) as u8;
        let rm = (opcode & 0xF) as u8;

        let a = self.reg(rm) as i32;
        let b = self.reg(rn) as i32;

        let (result, sat) = match kind {
            0b00 => signed_sat_add(a, b),                       // QADD
            0b01 => signed_sat_sub(a, b),                       // QSUB
            0b10 => {                                           // QDADD
                let (db, sat1) = signed_sat_double(b);
                let (sum, sat2) = signed_sat_add(a, db);
                (sum, sat1 || sat2)
            }
            0b11 => {                                           // QDSUB
                let (db, sat1) = signed_sat_double(b);
                let (diff, sat2) = signed_sat_sub(a, db);
                (diff, sat1 || sat2)
            }
            _ => unreachable!(),
        };

        self.regs[rd as usize] = result as u32;
        if sat {
            self.cpsr.set_q(true);
        }
        2
    }

    // ─── DSP Multiply: SMLA*/SMLAW*/SMLAL*/SMUL*/SMULW* ──────────

    fn arm_dsp_multiply(&mut self, opcode: u32) -> u32 {
        let op = (opcode >> 21) & 0x3;
        let rd = ((opcode >> 16) & 0xF) as u8;
        let rn = ((opcode >> 12) & 0xF) as u8;
        let rs = ((opcode >> 8) & 0xF) as u8;
        let rm = (opcode & 0xF) as u8;
        let x = (opcode >> 5) & 1 != 0; // selects high (1) or low (0) half of Rm
        let y = (opcode >> 6) & 1 != 0; // selects high (1) or low (0) half of Rs

        let half = |word: u32, top: bool| -> i32 {
            if top { (word as i32) >> 16 } else { (word as i16) as i32 }
        };

        match op {
            0b00 => {
                // SMLA<x><y>: Rd = (Rm.half * Rs.half) + Rn (saturating accumulate)
                let prod = (half(self.reg(rm), x) as i64) * (half(self.reg(rs), y) as i64);
                let acc = self.reg(rn) as i32;
                let (sum, sat) = signed_sat_add(prod as i32, acc);
                self.regs[rd as usize] = sum as u32;
                if sat { self.cpsr.set_q(true); }
            }
            0b01 => {
                // Either SMLAW<y> (bit 5 = 0) or SMULW<y> (bit 5 = 1).
                let rs_half = half(self.reg(rs), y) as i64;
                let prod = ((self.reg(rm) as i32 as i64) * rs_half) >> 16;
                if !x {
                    // SMLAW<y>: Rd = (Rm * Rs.half) >> 16 + Rn
                    let acc = self.reg(rn) as i32;
                    let (sum, sat) = signed_sat_add(prod as i32, acc);
                    self.regs[rd as usize] = sum as u32;
                    if sat { self.cpsr.set_q(true); }
                } else {
                    // SMULW<y>: Rd = (Rm * Rs.half) >> 16
                    self.regs[rd as usize] = prod as u32;
                }
            }
            0b10 => {
                // SMLAL<x><y>: 64-bit accumulate
                let prod = (half(self.reg(rm), x) as i64) * (half(self.reg(rs), y) as i64);
                let acc = ((self.reg(rd) as u64) << 32) | (self.reg(rn) as u64);
                let result = (acc as i64).wrapping_add(prod) as u64;
                self.regs[rn as usize] = result as u32;
                self.regs[rd as usize] = (result >> 32) as u32;
            }
            0b11 => {
                // SMUL<x><y>: Rd = Rm.half * Rs.half (no saturation, no accumulate)
                let prod = (half(self.reg(rm), x) as i64) * (half(self.reg(rs), y) as i64);
                self.regs[rd as usize] = prod as u32;
            }
            _ => unreachable!(),
        }

        3
    }

    // ─── Coprocessor MCR/MRC/CDP (CP15 only on NDS) ──────────────

    fn arm_coprocessor(&mut self, opcode: u32) -> u32 {
        let cp_num = (opcode >> 8) & 0xF;
        if cp_num != 15 {
            return self.arm_undefined(opcode);
        }

        let bit_4 = opcode & (1 << 4) != 0;
        if !bit_4 {
            // CDP — no architectural state on CP15. NOP.
            return 1;
        }

        let l = opcode & (1 << 20) != 0; // 1 = MRC, 0 = MCR
        let crn = (opcode >> 16) & 0xF;
        let rd = ((opcode >> 12) & 0xF) as u8;
        let crm = opcode & 0xF;
        let op1 = (opcode >> 21) & 0x7;
        let op2 = (opcode >> 5) & 0x7;

        if l {
            // MRC: read CP15 → Rd
            let val = self.cp15.read(crn, crm, op1, op2);
            if rd == 15 {
                // Encoding quirk: Rd=15 with MRC writes flags from val[31:28]
                self.cpsr.bits = (self.cpsr.bits & 0x0FFF_FFFF) | (val & 0xF000_0000);
            } else {
                self.regs[rd as usize] = val;
            }
        } else {
            // MCR: write Rd → CP15
            let val = self.reg(rd);
            self.cp15.write(crn, crm, op1, op2, val);
            // Refresh derived state on the CPU side (exception base).
            self.refresh_exception_base();
        }

        2
    }

    // ─── SWP / SWPB ──────────────────────────────────────────────

    fn arm_swap<B: CpuBus>(&mut self, bus: &mut B, opcode: u32) -> u32 {
        let b = opcode & (1 << 22) != 0;
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rd = ((opcode >> 12) & 0xF) as u8;
        let rm = (opcode & 0xF) as u8;

        let addr = self.reg(rn);

        if b {
            let old = bus.read8(addr) as u32;
            bus.write8(addr, self.reg(rm) as u8);
            self.regs[rd as usize] = old;
        } else {
            let aligned = addr & !3;
            let old = bus.read32(aligned);
            let rotation = (addr & 3) * 8;
            let old_rotated = old.rotate_right(rotation);
            bus.write32(aligned, self.reg(rm));
            self.regs[rd as usize] = old_rotated;
        }
        4
    }

    // ─── MRS ─────────────────────────────────────────────────────

    fn arm_mrs(&mut self, opcode: u32) -> u32 {
        let spsr = opcode & (1 << 22) != 0;
        let rd = ((opcode >> 12) & 0xF) as u8;
        let psr = if spsr { self.spsr() } else { self.cpsr };
        self.regs[rd as usize] = psr.bits;
        1
    }

    // ─── MSR ─────────────────────────────────────────────────────

    fn arm_msr(&mut self, opcode: u32) -> u32 {
        let i = opcode & (1 << 25) != 0;
        let spsr = opcode & (1 << 22) != 0;

        let field_mask = (opcode >> 16) & 0xF;
        let mut mask = 0u32;
        if field_mask & 1 != 0 { mask |= 0x0000_00FF; }
        if field_mask & 2 != 0 { mask |= 0x0000_FF00; }
        if field_mask & 4 != 0 { mask |= 0x00FF_0000; }
        if field_mask & 8 != 0 { mask |= 0xFF00_0000; }

        if self.cpsr.mode() == CpuMode::User {
            mask &= 0xFF00_0000;
        }

        let val = if i {
            let imm = opcode & 0xFF;
            let rotate = ((opcode >> 8) & 0xF) * 2;
            imm.rotate_right(rotate)
        } else {
            let rm = (opcode & 0xF) as u8;
            self.reg(rm)
        };

        if spsr {
            let mut psr = self.spsr();
            psr.bits = (psr.bits & !mask) | (val & mask);
            self.set_spsr(psr);
        } else {
            let old_mode = self.cpsr.mode();
            let new_bits = (self.cpsr.bits & !mask) | (val & mask);
            let new_mode = super::Psr { bits: new_bits }.mode();
            if old_mode != new_mode {
                self.switch_mode(new_mode);
            }
            self.cpsr.bits = new_bits;
        }
        1
    }

    // ─── SWI ─────────────────────────────────────────────────────

    fn arm_swi(&mut self, opcode: u32) -> u32 {
        let comment = (opcode >> 16) & 0xFF;
        self.pending_swi = Some(comment as u8);
        3
    }

    // ─── Undefined ───────────────────────────────────────────────

    fn arm_undefined(&mut self, opcode: u32) -> u32 {
        log::warn!("ARM undefined: 0x{:08X} at PC=0x{:08X}", opcode, self.regs[15].wrapping_sub(8));
        self.undefined_instruction();
        3
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestBus;
    use super::*;

    fn arm7_with(mem: usize) -> (Cpu, TestBus) {
        let mut cpu = Cpu::new_arm7();
        cpu.cpsr = super::super::Psr::new(super::CpuMode::System);
        cpu.cpsr.bits &= !(1 << 7);
        let bus = TestBus::new(mem);
        (cpu, bus)
    }

    fn arm9_with(mem: usize) -> (Cpu, TestBus) {
        let mut cpu = Cpu::new_arm9();
        cpu.cpsr = super::super::Psr::new(super::CpuMode::System);
        cpu.cpsr.bits &= !(1 << 7);
        let bus = TestBus::new(mem);
        (cpu, bus)
    }

    #[test]
    fn test_arm_mov_immediate() {
        let (mut cpu, mut bus) = arm7_with(0x100);
        cpu.execute_arm(&mut bus, 0xE3A0_002A); // MOV R0, #42
        assert_eq!(cpu.regs[0], 42);
    }

    #[test]
    fn test_arm_add() {
        let (mut cpu, mut bus) = arm7_with(0x100);
        cpu.regs[1] = 10;
        cpu.regs[2] = 20;
        cpu.execute_arm(&mut bus, 0xE081_0002); // ADD R0, R1, R2
        assert_eq!(cpu.regs[0], 30);
    }

    #[test]
    fn test_arm_str_ldr() {
        let (mut cpu, mut bus) = arm7_with(0x100);
        cpu.regs[0] = 0xDEAD_BEEF;
        cpu.regs[1] = 0x40;
        cpu.execute_arm(&mut bus, 0xE581_0000); // STR R0, [R1]
        cpu.execute_arm(&mut bus, 0xE591_2000); // LDR R2, [R1]
        assert_eq!(cpu.regs[2], 0xDEAD_BEEF);
    }

    #[test]
    fn test_arm_multiply() {
        let (mut cpu, mut bus) = arm7_with(0x100);
        cpu.regs[0] = 7;
        cpu.regs[1] = 6;
        cpu.execute_arm(&mut bus, 0xE002_0190); // MUL R2, R0, R1
        assert_eq!(cpu.regs[2], 42);
    }

    // ─── ARMv5TE additions (ARM9 only) ───────────────────────────

    #[test]
    fn test_clz_zero_input_returns_32() {
        let (mut cpu, mut bus) = arm9_with(0x100);
        cpu.regs[1] = 0;
        // CLZ R0, R1 → cond=AL, opcode = 0xE16F_0F11
        cpu.execute_arm(&mut bus, 0xE16F_0F11);
        assert_eq!(cpu.regs[0], 32);
    }

    #[test]
    fn test_clz_counts_leading_zeros() {
        let (mut cpu, mut bus) = arm9_with(0x100);
        cpu.regs[1] = 0x0000_0FFF;
        cpu.execute_arm(&mut bus, 0xE16F_0F11);
        assert_eq!(cpu.regs[0], 20);
    }

    #[test]
    fn test_clz_undefined_on_arm7() {
        let (mut cpu, mut bus) = arm7_with(0x100);
        cpu.regs[1] = 0xFFFF_FFFF;
        cpu.regs[0] = 0xCCCC_CCCC;
        cpu.execute_arm(&mut bus, 0xE16F_0F11);
        // ARM7 should NOT have written R0 — undefined exception entered.
        assert_eq!(cpu.regs[0], 0xCCCC_CCCC);
        assert_eq!(cpu.cpsr.mode(), super::CpuMode::Undefined);
    }

    #[test]
    fn test_qadd_saturates_positive() {
        let (mut cpu, mut bus) = arm9_with(0x100);
        cpu.regs[0] = i32::MAX as u32; // Rm
        cpu.regs[1] = 1;               // Rn
        // QADD R2, R0, R1 → cond=AL, op=0x10, opcode 7:4 = 0x5
        // Encoding: cond[31:28]=E, [27:20]=0x10, [19:16]=Rn=1, [15:12]=Rd=2, [11:8]=0, [7:4]=5, [3:0]=Rm=0
        let op = 0xE101_2050;
        cpu.execute_arm(&mut bus, op);
        assert_eq!(cpu.regs[2], i32::MAX as u32);
        assert!(cpu.cpsr.q());
    }

    #[test]
    fn test_qsub_no_saturation() {
        let (mut cpu, mut bus) = arm9_with(0x100);
        cpu.regs[0] = 10; // Rm
        cpu.regs[1] = 3;  // Rn
        cpu.cpsr.set_q(false);
        // QSUB R2, Rm=R0, Rn=R1 → Rd = SAT(Rm - Rn) = 7
        // bits[27:20]=0x12, bits[19:16]=Rn=1, bits[15:12]=Rd=2, bits[7:4]=5, bits[3:0]=Rm=0
        let op = 0xE121_2050;
        cpu.execute_arm(&mut bus, op);
        assert_eq!(cpu.regs[2], 7);
        assert!(!cpu.cpsr.q());
    }

    #[test]
    fn test_qsub_saturates_negative() {
        let (mut cpu, mut bus) = arm9_with(0x100);
        cpu.regs[0] = i32::MIN as u32; // Rm
        cpu.regs[1] = 1;                // Rn
        cpu.cpsr.set_q(false);
        let op = 0xE121_2050;
        cpu.execute_arm(&mut bus, op);
        assert_eq!(cpu.regs[2], i32::MIN as u32);
        assert!(cpu.cpsr.q());
    }

    #[test]
    fn test_smulxy_signed_halfword_multiply() {
        let (mut cpu, mut bus) = arm9_with(0x100);
        cpu.regs[0] = 0x0000_0003; // Rm low half = 3
        cpu.regs[1] = 0x0000_0005; // Rs low half = 5
        // SMULBB R2, R0, R1 (op=11, x=0 (B), y=0 (B))
        // bits[27:20] = 0x16, bits[7:4] = 0b1000 = 8
        // [31:28]=E, [27:20]=0x16, [19:16]=Rd=2, [15:12]=0, [11:8]=Rs=1, [7:4]=8, [3:0]=Rm=0
        let op = 0xE162_0180;
        cpu.execute_arm(&mut bus, op);
        assert_eq!(cpu.regs[2], 15);
    }

    #[test]
    fn test_blx_register_arm9() {
        let (mut cpu, mut bus) = arm9_with(0x100);
        cpu.regs[15] = 0x0800_0008; // PC = exec+8
        cpu.regs[0] = 0x40 | 1;     // target with thumb bit
        // BLX R0: cond=AL, [27:20]=0x12, [19:16]=F, [15:12]=F, [11:8]=F, [7:4]=3, [3:0]=Rm=0
        let op = 0xE12F_FF30;
        cpu.execute_arm(&mut bus, op);
        assert_eq!(cpu.regs[14], 0x0800_0004); // LR = PC - 4 (next instr)
        assert_eq!(cpu.regs[15], 0x40);
        assert!(cpu.cpsr.thumb());
    }

    #[test]
    fn test_blx_immediate_arm9() {
        let (mut cpu, mut bus) = arm9_with(0x100);
        cpu.regs[15] = 0x0800_0008;
        // BLX +4 (H=0, offset=1 word): cond=F, [27:25]=0b101, H=0, offset=1
        // 0xFA00_0001
        cpu.execute_arm(&mut bus, 0xFA00_0001);
        // Target = PC + 4 + (1<<2) = 0x0800_000C; H bit = 0 means no halfword adjust
        assert_eq!(cpu.regs[14], 0x0800_0004);
        assert!(cpu.cpsr.thumb());
    }
}

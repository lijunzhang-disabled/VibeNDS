//! ARM CPU core. Serves both the ARM946E-S (ARM9) and ARM7TDMI (ARM7) on
//! the NDS via an `is_arm9` flag.
//!
//! Ported from `../gba/gba-core/src/arm7tdmi/mod.rs` with these changes:
//! - The bus is abstracted via the `CpuBus` trait so the same `Cpu` can drive
//!   either `Bus9` or `Bus7`.
//! - `is_arm9` flag gates ARMv5TE encodings.
//! - IRQ entry honors the configurable exception base address (set to
//!   `0xFFFF_0000` on ARM9 by CP15 c1 bit 13; `0x0000_0000` on ARM7).

pub mod alu;
pub mod arm;
pub mod bus;
pub mod cp15;
pub mod thumb;

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub use bus::CpuBus;

/// ARM CPU operating modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CpuMode {
    User = 0x10,
    Fiq = 0x11,
    Irq = 0x12,
    Supervisor = 0x13,
    Abort = 0x17,
    Undefined = 0x1B,
    System = 0x1F,
}

impl CpuMode {
    pub fn from_bits(bits: u32) -> Self {
        match bits & 0x1F {
            0x10 => CpuMode::User,
            0x11 => CpuMode::Fiq,
            0x12 => CpuMode::Irq,
            0x13 => CpuMode::Supervisor,
            0x17 => CpuMode::Abort,
            0x1B => CpuMode::Undefined,
            0x1F => CpuMode::System,
            _ => CpuMode::User,
        }
    }

    pub fn bank_index(self) -> usize {
        match self {
            CpuMode::User | CpuMode::System => 0,
            CpuMode::Fiq => 1,
            CpuMode::Irq => 2,
            CpuMode::Supervisor => 3,
            CpuMode::Abort => 4,
            CpuMode::Undefined => 5,
        }
    }

    pub fn has_spsr(self) -> bool {
        !matches!(self, CpuMode::User | CpuMode::System)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Psr {
    pub bits: u32,
}

impl Psr {
    pub fn new(mode: CpuMode) -> Self {
        Psr {
            bits: mode as u32 | (1 << 7) | (1 << 6),
        }
    }

    #[inline]
    pub fn n(self) -> bool {
        self.bits >> 31 != 0
    }
    #[inline]
    pub fn z(self) -> bool {
        (self.bits >> 30) & 1 != 0
    }
    #[inline]
    pub fn c(self) -> bool {
        (self.bits >> 29) & 1 != 0
    }
    #[inline]
    pub fn v(self) -> bool {
        (self.bits >> 28) & 1 != 0
    }
    #[inline]
    pub fn q(self) -> bool {
        (self.bits >> 27) & 1 != 0
    }
    #[inline]
    pub fn irq_disabled(self) -> bool {
        (self.bits >> 7) & 1 != 0
    }
    #[inline]
    pub fn fiq_disabled(self) -> bool {
        (self.bits >> 6) & 1 != 0
    }
    #[inline]
    pub fn thumb(self) -> bool {
        (self.bits >> 5) & 1 != 0
    }
    #[inline]
    pub fn mode(self) -> CpuMode {
        CpuMode::from_bits(self.bits)
    }

    #[inline]
    pub fn set_n(&mut self, v: bool) {
        self.bits = (self.bits & !(1 << 31)) | ((v as u32) << 31);
    }
    #[inline]
    pub fn set_z(&mut self, v: bool) {
        self.bits = (self.bits & !(1 << 30)) | ((v as u32) << 30);
    }
    #[inline]
    pub fn set_c(&mut self, v: bool) {
        self.bits = (self.bits & !(1 << 29)) | ((v as u32) << 29);
    }
    #[inline]
    pub fn set_v(&mut self, v: bool) {
        self.bits = (self.bits & !(1 << 28)) | ((v as u32) << 28);
    }
    #[inline]
    pub fn set_q(&mut self, v: bool) {
        self.bits = (self.bits & !(1 << 27)) | ((v as u32) << 27);
    }

    #[inline]
    pub fn set_nz(&mut self, result: u32) {
        self.set_n(result >> 31 != 0);
        self.set_z(result == 0);
    }

    pub fn set_thumb(&mut self, v: bool) {
        self.bits = (self.bits & !(1 << 5)) | ((v as u32) << 5);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankedRegisters {
    pub(crate) sp: [u32; 6],
    pub(crate) lr: [u32; 6],
    fiq_r8_r12: [u32; 5],
    usr_r8_r12: [u32; 5],
    spsr: [Psr; 5],
}

impl BankedRegisters {
    pub fn new() -> Self {
        BankedRegisters {
            sp: [0; 6],
            lr: [0; 6],
            fiq_r8_r12: [0; 5],
            usr_r8_r12: [0; 5],
            spsr: [Psr { bits: 0 }; 5],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cpu {
    pub regs: [u32; 16],
    pub cpsr: Psr,
    pub(crate) banked: BankedRegisters,
    pipeline: [u32; 2],
    pub pipeline_flushed: bool,
    pub halted: bool,
    /// IntrWait mask. `0` = HALTCNT-style wake (any pending IRQ clears `halted`).
    /// Non-zero = SWI 0x04 / 0x05 semantics: only an IRQ whose bit is in the
    /// mask clears `halted`. Cleared automatically on wake. Real BIOS
    /// implements this as a HALT-recheck loop; we collapse the loop into a
    /// gated halt-wake check. See `debug/2026-05-29_intrwait-mask-inherited.md`.
    #[serde(default)]
    pub intrwait_mask: u32,
    pub(crate) pending_swi: Option<u8>,

    /// True for the ARM946E-S core; false for ARM7TDMI. Gates ARMv5TE
    /// encodings — when false, any ARMv5TE-only opcode raises an undefined
    /// instruction exception (logged-only for now).
    pub is_arm9: bool,

    /// Exception base address. `0x0000_0000` for ARM7 and for ARM9 in low-vector
    /// mode; `0xFFFF_0000` for ARM9 in high-vector mode (CP15 c1 bit 13).
    pub exception_base: u32,

    /// CP15 system control coprocessor — only meaningful when `is_arm9` is true.
    pub cp15: cp15::Cp15,

    #[serde(default)]
    pub irq_entries: u64,
}

impl Cpu {
    /// Build an ARM7TDMI-style CPU (ARM7 on the NDS).
    pub fn new_arm7() -> Self {
        let mut cpu = Cpu::new_common(false);
        cpu.regs[15] = 0x0000_0000;
        cpu
    }

    /// Build an ARM946E-S-style CPU (ARM9 on the NDS). Starts in high-vector
    /// mode by default to match the NDS's ARM9 BIOS at `0xFFFF_0000`.
    pub fn new_arm9() -> Self {
        let mut cpu = Cpu::new_common(true);
        cpu.exception_base = 0xFFFF_0000;
        cpu.cp15.set_high_vectors(true);
        cpu.regs[15] = 0xFFFF_0000;
        cpu
    }

    fn new_common(is_arm9: bool) -> Self {
        Cpu {
            regs: [0; 16],
            cpsr: Psr::new(CpuMode::Supervisor),
            banked: BankedRegisters::new(),
            pipeline: [0; 2],
            pipeline_flushed: true,
            halted: false,
            intrwait_mask: 0,
            pending_swi: None,
            is_arm9,
            exception_base: 0,
            cp15: cp15::Cp15::new(),
            irq_entries: 0,
        }
    }

    /// Step one instruction. Returns the cycle count consumed in this CPU's
    /// own clock domain (not normalized).
    ///
    /// Pipeline refill runs in **two** places:
    ///
    /// 1. *Before* the IRQ check, so `handle_interrupt` reads `regs[15]`
    ///    at its correct pipeline-ahead value (`exec + 4` in THUMB,
    ///    `exec + 8` in ARM). Without this, an IRQ that lands in the
    ///    one-step window after a branch saves `LR_irq` from a stale
    ///    `regs[15] = raw_target`, and the BIOS's `SUBS PC, LR, #4`
    ///    return resumes at `target − 4` (or − 8 in ARM) — landing
    ///    inside the previous instruction. Inherited from the GBA port
    ///    pre-fix; see `debug/2026-04-27_irq-pipeline-refill-inherited.md`.
    ///
    /// 2. *After* the IRQ check, because IRQ entry itself flushes the
    ///    pipeline (it sets `regs[15]` to the vector address). The
    ///    refill must happen so the next decode reads from the vector,
    ///    not from a half-stale pipeline.
    ///
    /// Both refills are guarded by `pipeline_flushed` and are idempotent.
    pub fn step<B: CpuBus>(&mut self, bus: &mut B) -> u32 {
        if self.pipeline_flushed {
            self.refill_pipeline(bus);
        }

        if bus.irq_pending() && !self.cpsr.irq_disabled() {
            self.irq_entries += 1;
            self.handle_interrupt();
            self.halted = false;
            self.intrwait_mask = 0;
        }

        if self.halted {
            return 1;
        }

        if self.pipeline_flushed {
            self.refill_pipeline(bus);
        }

        trace_arm9_exec(self);

        if self.cpsr.thumb() {
            self.step_thumb(bus)
        } else {
            self.step_arm(bus)
        }
    }

    fn step_arm<B: CpuBus>(&mut self, bus: &mut B) -> u32 {
        let opcode = self.pipeline[0];

        if !self.check_condition(opcode >> 28) {
            // ARM7TDMI treats condition 0xF as NV ("never"). ARM9 uses that
            // space for ARMv5 unconditional encodings and routes it below.
            self.advance_arm_pipeline(bus);
            return 1;
        }

        let cycles = self.execute_arm(bus, opcode);

        if !self.pipeline_flushed {
            self.advance_arm_pipeline(bus);
        }

        cycles
    }

    fn step_thumb<B: CpuBus>(&mut self, bus: &mut B) -> u32 {
        let opcode = self.pipeline[0] as u16;
        let cycles = self.execute_thumb(bus, opcode);

        if !self.pipeline_flushed {
            self.advance_thumb_pipeline(bus);
        }

        cycles
    }

    #[inline]
    fn advance_arm_pipeline<B: CpuBus>(&mut self, bus: &mut B) {
        self.pipeline[0] = self.pipeline[1];
        self.pipeline[1] = bus.read32(self.regs[15]);
        self.regs[15] = self.regs[15].wrapping_add(4);
    }

    #[inline]
    fn advance_thumb_pipeline<B: CpuBus>(&mut self, bus: &mut B) {
        self.pipeline[0] = self.pipeline[1];
        self.pipeline[1] = bus.read16(self.regs[15]) as u32;
        self.regs[15] = self.regs[15].wrapping_add(2);
    }

    fn refill_pipeline<B: CpuBus>(&mut self, bus: &mut B) {
        if self.cpsr.thumb() {
            let pc = self.regs[15] & !1;
            self.pipeline[0] = bus.read16(pc) as u32;
            self.pipeline[1] = bus.read16(pc + 2) as u32;
            self.regs[15] = pc.wrapping_add(4);
        } else {
            let pc = self.regs[15] & !3;
            self.pipeline[0] = bus.read32(pc);
            self.pipeline[1] = bus.read32(pc.wrapping_add(4));
            self.regs[15] = pc.wrapping_add(8);
        }
        self.pipeline_flushed = false;
    }

    pub(crate) fn check_condition(&self, cond: u32) -> bool {
        match cond & 0xF {
            0x0 => self.cpsr.z(),
            0x1 => !self.cpsr.z(),
            0x2 => self.cpsr.c(),
            0x3 => !self.cpsr.c(),
            0x4 => self.cpsr.n(),
            0x5 => !self.cpsr.n(),
            0x6 => self.cpsr.v(),
            0x7 => !self.cpsr.v(),
            0x8 => self.cpsr.c() && !self.cpsr.z(),
            0x9 => !self.cpsr.c() || self.cpsr.z(),
            0xA => self.cpsr.n() == self.cpsr.v(),
            0xB => self.cpsr.n() != self.cpsr.v(),
            0xC => !self.cpsr.z() && (self.cpsr.n() == self.cpsr.v()),
            0xD => self.cpsr.z() || (self.cpsr.n() != self.cpsr.v()),
            0xE => true,
            0xF => self.is_arm9, // ARMv5 unconditional on ARM9; NV on ARM7.
            _ => unreachable!(),
        }
    }

    pub fn switch_mode(&mut self, new_mode: CpuMode) {
        let old_mode = self.cpsr.mode();
        if old_mode == new_mode {
            return;
        }

        let old_bank = old_mode.bank_index();
        self.banked.sp[old_bank] = self.regs[13];
        self.banked.lr[old_bank] = self.regs[14];

        if old_mode == CpuMode::Fiq {
            self.banked.fiq_r8_r12.copy_from_slice(&self.regs[8..13]);
            self.regs[8..13].copy_from_slice(&self.banked.usr_r8_r12);
        } else if new_mode == CpuMode::Fiq {
            self.banked.usr_r8_r12.copy_from_slice(&self.regs[8..13]);
            self.regs[8..13].copy_from_slice(&self.banked.fiq_r8_r12);
        }

        let new_bank = new_mode.bank_index();
        self.regs[13] = self.banked.sp[new_bank];
        self.regs[14] = self.banked.lr[new_bank];

        self.cpsr.bits = (self.cpsr.bits & !0x1F) | (new_mode as u32);
    }

    pub fn spsr(&self) -> Psr {
        let mode = self.cpsr.mode();
        if mode.has_spsr() {
            let index = match mode {
                CpuMode::Fiq => 0,
                CpuMode::Irq => 1,
                CpuMode::Supervisor => 2,
                CpuMode::Abort => 3,
                CpuMode::Undefined => 4,
                _ => return self.cpsr,
            };
            self.banked.spsr[index]
        } else {
            self.cpsr
        }
    }

    pub fn set_spsr(&mut self, psr: Psr) {
        let mode = self.cpsr.mode();
        if mode.has_spsr() {
            let index = match mode {
                CpuMode::Fiq => 0,
                CpuMode::Irq => 1,
                CpuMode::Supervisor => 2,
                CpuMode::Abort => 3,
                CpuMode::Undefined => 4,
                _ => return,
            };
            self.banked.spsr[index] = psr;
        }
    }

    /// Refresh the exception base from CP15. The ARM9 BIOS sets bit 13 of
    /// CP15 c1 at boot, and the value latches `0xFFFF_0000` here.
    pub fn refresh_exception_base(&mut self) {
        if self.is_arm9 {
            self.exception_base = if self.cp15.high_vectors() {
                0xFFFF_0000
            } else {
                0x0000_0000
            };
        } else {
            self.exception_base = 0;
        }
    }

    fn handle_interrupt(&mut self) {
        let return_addr = if self.cpsr.thumb() {
            self.regs[15]
        } else {
            self.regs[15].wrapping_sub(4)
        };

        let saved_cpsr = self.cpsr;
        self.switch_mode(CpuMode::Irq);
        self.set_spsr(saved_cpsr);

        self.regs[14] = return_addr;
        self.cpsr.set_thumb(false);
        self.cpsr.bits |= 1 << 7;

        self.regs[15] = self.exception_base.wrapping_add(0x18);
        self.pipeline_flushed = true;
    }

    pub fn software_interrupt(&mut self, _comment: u32) {
        let return_addr = if self.cpsr.thumb() {
            self.regs[15].wrapping_sub(2)
        } else {
            self.regs[15].wrapping_sub(4)
        };

        let saved_cpsr = self.cpsr;
        self.switch_mode(CpuMode::Supervisor);
        self.set_spsr(saved_cpsr);

        self.regs[14] = return_addr;
        self.cpsr.set_thumb(false);
        self.cpsr.bits |= 1 << 7;

        self.regs[15] = self.exception_base.wrapping_add(0x08);
        self.pipeline_flushed = true;
    }

    pub fn undefined_instruction(&mut self) {
        let return_addr = if self.cpsr.thumb() {
            self.regs[15].wrapping_sub(2)
        } else {
            self.regs[15].wrapping_sub(4)
        };

        let saved_cpsr = self.cpsr;
        self.switch_mode(CpuMode::Undefined);
        self.set_spsr(saved_cpsr);

        self.regs[14] = return_addr;
        self.cpsr.set_thumb(false);
        self.cpsr.bits |= 1 << 7;

        self.regs[15] = self.exception_base.wrapping_add(0x04);
        self.pipeline_flushed = true;
    }

    #[inline]
    pub fn branch(&mut self, addr: u32) {
        self.regs[15] = addr;
        self.pipeline_flushed = true;
    }

    /// Branch and exchange — switches state via bit 0 of `addr`.
    #[inline]
    pub fn branch_exchange(&mut self, addr: u32) {
        self.cpsr.set_thumb(addr & 1 != 0);
        self.regs[15] = addr & !1;
        self.pipeline_flushed = true;
    }

    #[inline]
    pub fn reg(&self, r: u8) -> u32 {
        self.regs[r as usize & 0xF]
    }

    #[inline]
    pub fn set_reg(&mut self, r: u8, val: u32) {
        let r = r as usize & 0xF;
        if r == 15 {
            // ARMv5+ interworks on register-write to PC: bit 0 selects state.
            // ARMv4 (ARM7) ignores bit 0. We branch_exchange on ARM9 and
            // branch on ARM7.
            if self.is_arm9 {
                self.branch_exchange(val);
            } else {
                self.branch(val & !1);
            }
        } else {
            self.regs[r] = val;
        }
    }

    pub fn read_user_reg(&self, r: u8) -> u32 {
        let r = r as usize & 0xF;
        let mode = self.cpsr.mode();
        match r {
            0..=7 | 15 => self.regs[r],
            8..=12 => {
                if mode == CpuMode::Fiq {
                    self.banked.usr_r8_r12[r - 8]
                } else {
                    self.regs[r]
                }
            }
            13 => {
                if mode.bank_index() == 0 {
                    self.regs[13]
                } else {
                    self.banked.sp[0]
                }
            }
            14 => {
                if mode.bank_index() == 0 {
                    self.regs[14]
                } else {
                    self.banked.lr[0]
                }
            }
            _ => unreachable!(),
        }
    }

    pub fn write_user_reg(&mut self, r: u8, val: u32) {
        let r = r as usize & 0xF;
        let mode = self.cpsr.mode();
        match r {
            0..=7 => self.regs[r] = val,
            15 => self.regs[r] = val,
            8..=12 => {
                if mode == CpuMode::Fiq {
                    self.banked.usr_r8_r12[r - 8] = val;
                } else {
                    self.regs[r] = val;
                }
            }
            13 => {
                if mode.bank_index() == 0 {
                    self.regs[13] = val;
                } else {
                    self.banked.sp[0] = val;
                }
            }
            14 => {
                if mode.bank_index() == 0 {
                    self.regs[14] = val;
                } else {
                    self.banked.lr[0] = val;
                }
            }
            _ => unreachable!(),
        }
    }

    pub fn set_reg_with_flags(&mut self, r: u8, val: u32, s: bool) {
        let r_idx = r as usize & 0xF;
        if r_idx == 15 {
            if s {
                let spsr = self.spsr();
                let new_mode = spsr.mode();
                self.switch_mode(new_mode);
                self.cpsr = spsr;
                self.branch(val & !1);
                return;
            }
            // On ordinary ARMv5 PC writes, the result is interworked; on
            // exception returns (`S` set), CPSR.T came from SPSR and must not
            // be overwritten by bit 0 of the restored PC value.
            if self.is_arm9 {
                self.branch_exchange(val);
            } else {
                self.branch(val & !1);
            }
        } else {
            self.regs[r_idx] = val;
        }
    }
}

fn trace_arm9_exec(cpu: &Cpu) {
    if !cpu.is_arm9 {
        return;
    }
    let pc = if cpu.cpsr.thumb() {
        cpu.regs[15].wrapping_sub(4) & !1
    } else {
        cpu.regs[15].wrapping_sub(8) & !3
    };
    if let Some(pcs) = trace_arm9_exec_pcs() {
        if !pcs.contains(&pc) {
            return;
        }
    } else {
        let Some((start, end)) = trace_arm9_exec_range() else {
            return;
        };
        if pc < start || pc >= end {
            return;
        }
    }
    if let Some((start, end)) = trace_arm9_exec_reg_range() {
        if !cpu.regs.iter().any(|reg| *reg >= start && *reg < end) {
            return;
        }
    }
    if let Some(want) = trace_arm9_exec_r2_value() {
        if cpu.regs[2] != want {
            return;
        }
    }

    eprint!(
        "arm9 exec pc=0x{pc:08X} thumb={} op=0x{:08X} cpsr=0x{:08X}",
        cpu.cpsr.thumb(),
        cpu.pipeline[0],
        cpu.cpsr.bits
    );
    for (i, reg) in cpu.regs.iter().enumerate() {
        eprint!(" r{i}=0x{reg:08X}");
    }
    eprintln!();
}

fn trace_arm9_exec_range() -> Option<(u32, u32)> {
    static RANGE: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *RANGE.get_or_init(|| parse_trace_range_env("NDS_TRACE_ARM9_EXEC_RANGE"))
}

fn trace_arm9_exec_reg_range() -> Option<(u32, u32)> {
    static RANGE: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *RANGE.get_or_init(|| parse_trace_range_env("NDS_TRACE_ARM9_EXEC_REG_RANGE"))
}

fn trace_arm9_exec_pcs() -> Option<&'static [u32]> {
    static PCS: OnceLock<Option<Vec<u32>>> = OnceLock::new();
    PCS.get_or_init(|| parse_trace_pc_list_env("NDS_TRACE_ARM9_EXEC_PCS"))
        .as_deref()
}

fn trace_arm9_exec_r2_value() -> Option<u32> {
    static VALUE: OnceLock<Option<u32>> = OnceLock::new();
    *VALUE.get_or_init(|| {
        let spec = std::env::var_os("NDS_TRACE_ARM9_EXEC_R2_VALUE")?;
        let spec = spec.to_str()?;
        u32::from_str_radix(spec.trim_start_matches("0x"), 16).ok()
    })
}

fn parse_trace_range_env(env: &str) -> Option<(u32, u32)> {
    let spec = std::env::var_os(env)?;
    let spec = spec.to_str()?;
    let (start, end) = spec.split_once("..")?;
    let start = u32::from_str_radix(start.trim_start_matches("0x"), 16).ok()?;
    let end = u32::from_str_radix(end.trim_start_matches("0x"), 16).ok()?;
    Some((start, end))
}

fn parse_trace_pc_list_env(env: &str) -> Option<Vec<u32>> {
    let spec = std::env::var_os(env)?;
    let spec = spec.to_str()?;
    let pcs: Vec<u32> = spec
        .split([',', ' ', '\t', '\n'])
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                None
            } else {
                u32::from_str_radix(part.trim_start_matches("0x"), 16).ok()
            }
        })
        .collect();
    if pcs.is_empty() {
        None
    } else {
        Some(pcs)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::bus::CpuBus;
    use super::*;

    /// A tiny RAM-only bus for unit tests.
    pub(crate) struct TestBus {
        pub mem: Vec<u8>,
        pub irq: bool,
    }

    impl TestBus {
        pub fn new(size: usize) -> Self {
            TestBus {
                mem: vec![0u8; size],
                irq: false,
            }
        }
    }

    impl CpuBus for TestBus {
        fn read8(&mut self, addr: u32) -> u8 {
            *self.mem.get(addr as usize).unwrap_or(&0)
        }
        fn read16(&mut self, addr: u32) -> u16 {
            let a = addr as usize;
            if a + 1 >= self.mem.len() {
                return 0;
            }
            u16::from_le_bytes([self.mem[a], self.mem[a + 1]])
        }
        fn read32(&mut self, addr: u32) -> u32 {
            let a = addr as usize;
            if a + 3 >= self.mem.len() {
                return 0;
            }
            u32::from_le_bytes([
                self.mem[a],
                self.mem[a + 1],
                self.mem[a + 2],
                self.mem[a + 3],
            ])
        }
        fn write8(&mut self, addr: u32, val: u8) {
            if let Some(b) = self.mem.get_mut(addr as usize) {
                *b = val;
            }
        }
        fn write16(&mut self, addr: u32, val: u16) {
            let bytes = val.to_le_bytes();
            let a = addr as usize;
            if a + 1 < self.mem.len() {
                self.mem[a] = bytes[0];
                self.mem[a + 1] = bytes[1];
            }
        }
        fn write32(&mut self, addr: u32, val: u32) {
            let bytes = val.to_le_bytes();
            let a = addr as usize;
            if a + 3 < self.mem.len() {
                for i in 0..4 {
                    self.mem[a + i] = bytes[i];
                }
            }
        }
        fn irq_pending(&self) -> bool {
            self.irq
        }
    }

    #[test]
    fn test_psr_flags() {
        let mut psr = Psr { bits: 0 };
        psr.set_n(true);
        assert!(psr.n());
        psr.set_z(true);
        assert!(psr.z());
        psr.set_c(true);
        assert!(psr.c());
        psr.set_v(true);
        assert!(psr.v());
        psr.set_q(true);
        assert!(psr.q());
        psr.set_nz(0);
        assert!(!psr.n());
        assert!(psr.z());
        psr.set_nz(0x80000000);
        assert!(psr.n());
        assert!(!psr.z());
    }

    #[test]
    fn test_condition_codes() {
        let cpu = Cpu::new_arm7();
        assert!(cpu.check_condition(0xE));
        assert!(!cpu.check_condition(0xF));

        let cpu = Cpu::new_arm9();
        assert!(cpu.check_condition(0xF));
    }

    #[test]
    fn test_mode_switching() {
        let mut cpu = Cpu::new_arm7();
        cpu.cpsr = Psr::new(CpuMode::System);
        cpu.regs[13] = 0x1234;
        cpu.regs[14] = 0x5678;
        cpu.switch_mode(CpuMode::Irq);
        assert_eq!(cpu.cpsr.mode(), CpuMode::Irq);
        assert_eq!(cpu.regs[13], 0);
        assert_eq!(cpu.regs[14], 0);
        cpu.switch_mode(CpuMode::System);
        assert_eq!(cpu.regs[13], 0x1234);
        assert_eq!(cpu.regs[14], 0x5678);
    }

    #[test]
    fn test_exception_return_pc_write_preserves_spsr_thumb_state() {
        let mut cpu = Cpu::new_arm9();
        cpu.cpsr = Psr::new(CpuMode::Irq);
        let mut spsr = Psr::new(CpuMode::System);
        spsr.set_thumb(true);
        cpu.set_spsr(spsr);

        cpu.set_reg_with_flags(15, 0x0200_1F4C, true);

        assert_eq!(cpu.cpsr.mode(), CpuMode::System);
        assert!(cpu.cpsr.thumb());
        assert_eq!(cpu.regs[15], 0x0200_1F4C);
    }

    #[test]
    fn test_arm9_starts_at_high_vectors() {
        let cpu = Cpu::new_arm9();
        assert!(cpu.is_arm9);
        assert_eq!(cpu.exception_base, 0xFFFF_0000);
        assert!(cpu.cp15.high_vectors());
    }

    #[test]
    fn test_arm7_starts_at_low_vectors() {
        let cpu = Cpu::new_arm7();
        assert!(!cpu.is_arm9);
        assert_eq!(cpu.exception_base, 0);
    }

    /// Regression for the IRQ-pipeline-refill ordering bug inherited from
    /// the GBA project at port time (see
    /// `debug/2026-04-27_irq-pipeline-refill-inherited.md`).
    ///
    /// Scenario (ARM mode):
    ///   0x100: B +0x10        → branches to 0x118
    ///   0x118: MOV R5, #0x42  (the post-branch instruction)
    ///   0x18:  SUBS PC, LR, #4 (planted IRQ handler — bare return)
    ///
    /// Pre-fix: at step #2, IRQ check reads stale `regs[15] = 0x118`
    ///   (raw branch target, no refill yet). `handle_interrupt` saves
    ///   `LR_irq = regs[15] - 4 = 0x114`. The handler's `SUBS PC, LR, #4`
    ///   returns via `LR_irq - 4 = 0x110` — inside the gap before the
    ///   target. Memory there is zero, decoded as `AND R0, R0, R0` with
    ///   cond=EQ (Z=false → skipped). R5 never gets 0x42.
    ///
    /// Post-fix: refill runs first, `regs[15] = 0x118 + 8 = 0x120`,
    ///   `LR_irq = 0x11C`, return resumes at 0x118 — correct. R5 = 0x42.
    #[test]
    fn test_irq_during_pipeline_flushed_window_resumes_at_branch_target() {
        let mut cpu = Cpu::new_arm7();
        cpu.cpsr = Psr::new(CpuMode::System);
        cpu.cpsr.bits &= !(1 << 7); // IRQ enabled

        let mut bus = TestBus::new(0x200);

        // 0x100: B +0x10 — encoded so target = 0x118.
        // ARM B math: target = (PC_at_exec + 8) + (signed_24 << 2).
        // (0x118 - (0x100 + 8)) >> 2 = 4 → opcode 0xEA00_0004.
        let b_op: u32 = 0xEA00_0004;
        bus.mem[0x100..0x104].copy_from_slice(&b_op.to_le_bytes());

        // 0x118: MOV R5, #0x42.
        let mov_op: u32 = 0xE3A0_5042;
        bus.mem[0x118..0x11C].copy_from_slice(&mov_op.to_le_bytes());

        // 0x18: IRQ vector — SUBS PC, LR, #4 (encoded 0xE25E_F004).
        let subs_pc_lr_4: u32 = 0xE25E_F004;
        bus.mem[0x18..0x1C].copy_from_slice(&subs_pc_lr_4.to_le_bytes());

        cpu.regs[15] = 0x100;
        cpu.pipeline_flushed = true;

        // ─── Step #1: execute the branch ─────────────────────────
        // Ends with regs[15] = 0x118 (raw target), pipeline_flushed = true.
        cpu.step(&mut bus);
        assert_eq!(cpu.regs[15], 0x118, "branch target should be raw 0x118");
        assert!(cpu.pipeline_flushed, "branch should leave pipeline flushed");

        // ─── Step #2: IRQ fires INSIDE the flushed window ────────
        // step() does: refill (post-fix) → handle_interrupt → refill at
        // vector → execute SUBS PC,LR,#4 → restore mode + branch back.
        // After this step, mode is back to System and regs[15] is the
        // resumed PC.
        bus.irq = true;
        cpu.step(&mut bus);
        bus.irq = false;

        // Sanity check: SPSR_irq, banked away during SUBS-return, should
        // still hold the saved LR_irq value (0x11C post-fix, 0x114 pre-fix).
        assert_eq!(
            cpu.banked.lr[CpuMode::Irq.bank_index()],
            0x11C,
            "banked LR_irq should be branch_target + 4 (= 0x11C); pre-fix \
             bug saves 0x114 (stale regs[15] - 4)"
        );

        assert_eq!(
            cpu.cpsr.mode(),
            CpuMode::System,
            "SUBS PC,LR,#4 should have restored System mode via SPSR"
        );
        assert_eq!(
            cpu.regs[15], 0x118,
            "resumed PC should be the branch target 0x118; pre-fix bug \
             would resume at 0x110 (inside the gap before the target)"
        );

        // ─── Step #3: execute MOV R5, #0x42 at the resumed PC ────
        cpu.step(&mut bus);
        assert_eq!(
            cpu.regs[5], 0x42,
            "MOV R5, #0x42 at branch target should set R5 — pre-fix bug \
             would have decoded zeros at 0x110 instead and never set R5"
        );
    }
}

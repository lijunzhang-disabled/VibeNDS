//! ARM7 BIOS HLE — most SWIs are identical to ARM9; sound-driver SWIs
//! (0x1A-0x21) are stubbed and rely on a real BIOS for full coverage.

use crate::cpu::{Cpu, CpuBus};
use super::common;

pub fn handle_swi<B: CpuBus>(cpu: &mut Cpu, bus: &mut B, comment: u8) -> bool {
    match comment {
        0x04 => { swi_intr_wait(cpu); true }
        0x05 => { swi_vblank_intr_wait(cpu); true }
        0x06 => { common::swi_div(cpu); true }
        0x08 => { common::swi_sqrt(cpu); true }
        0x0B => { common::swi_cpu_set(cpu, bus); true }
        0x0C => { common::swi_cpu_fast_set(cpu, bus); true }
        0x0D => { common::swi_get_crc16(cpu, bus); true }
        0x11 => { common::swi_lz77_uncomp(cpu, bus, false); true }
        0x12 => { common::swi_lz77_uncomp(cpu, bus, true); true }
        _ => {
            log::trace!("ARM7 unhandled SWI 0x{:02X}", comment);
            false
        }
    }
}

/// See `debug/2026-05-29_intrwait-mask-inherited.md` and the ARM9 sibling
/// for the rationale: real BIOS only wakes on a matching IRQ, so we gate
/// halt-wake on `cpu.intrwait_mask`.
fn swi_intr_wait(cpu: &mut Cpu) {
    let mask = cpu.regs[1];
    cpu.intrwait_mask = if mask != 0 { mask } else { 0xFFFF_FFFF };
    cpu.halted = true;
}

fn swi_vblank_intr_wait(cpu: &mut Cpu) {
    cpu.regs[0] = 1;
    cpu.regs[1] = 1;
    swi_intr_wait(cpu);
}

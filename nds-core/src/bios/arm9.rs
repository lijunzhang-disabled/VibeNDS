//! ARM9 BIOS HLE — handles SWIs the ARM9 BIOS exposes.

use crate::cpu::{Cpu, CpuBus};
use super::common;

/// Returns true if the SWI was handled.
pub fn handle_swi<B: CpuBus>(cpu: &mut Cpu, bus: &mut B, comment: u8) -> bool {
    match comment {
        0x04 => swi_intr_wait(cpu),
        0x05 => swi_vblank_intr_wait(cpu),
        0x06 => { common::swi_div(cpu); true }
        0x08 => { common::swi_sqrt(cpu); true }
        0x0B => { common::swi_cpu_set(cpu, bus); true }
        0x0C => { common::swi_cpu_fast_set(cpu, bus); true }
        0x0D => { common::swi_get_crc16(cpu, bus); true }
        0x11 => { common::swi_lz77_uncomp(cpu, bus, false); true }
        0x12 => { common::swi_lz77_uncomp(cpu, bus, true); true }
        _ => {
            log::trace!("ARM9 unhandled SWI 0x{:02X}", comment);
            false
        }
    }
}

/// Wait until any flag in R1 is set in IF (then auto-acknowledge it).
/// HLE shortcut: just halt the CPU. The IRQ controller's pending bit will
/// wake it; the kernel's IRQ handler clears IF. This works because we
/// already gate IRQ delivery on IF & IE & IME.
fn swi_intr_wait(cpu: &mut Cpu) -> bool {
    cpu.halted = true;
    true
}

fn swi_vblank_intr_wait(cpu: &mut Cpu) -> bool {
    cpu.regs[0] = 1;
    cpu.regs[1] = 1; // VBlank
    swi_intr_wait(cpu)
}

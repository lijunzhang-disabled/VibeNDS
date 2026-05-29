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

/// Wait until any flag in R1 is set in IF, then auto-acknowledge it.
///
/// Real BIOS implements this as a `loop { HALT; if BIOS_IF & mask: return }`
/// loop — only a *matching* IRQ wakes; any other IRQ re-enters HALT. We
/// collapse the loop into a gated halt-wake check using `cpu.intrwait_mask`.
/// A zero `R1` mask is treated as "wake on any IRQ" so the BIOS doesn't
/// park forever (matches `gba/debug/2026-05-24_fe7-hblank-irq-cascade.md`).
/// See `debug/2026-05-29_intrwait-mask-inherited.md`.
fn swi_intr_wait(cpu: &mut Cpu) -> bool {
    let mask = cpu.regs[1];
    cpu.intrwait_mask = if mask != 0 { mask } else { 0xFFFF_FFFF };
    cpu.halted = true;
    true
}

fn swi_vblank_intr_wait(cpu: &mut Cpu) -> bool {
    cpu.regs[0] = 1;
    cpu.regs[1] = 1; // VBlank
    swi_intr_wait(cpu)
}

//! ARM9 BIOS HLE — handles SWIs the ARM9 BIOS exposes.

use super::common;
use crate::cpu::{Cpu, CpuBus};

/// Returns true if the SWI was handled.
pub fn handle_swi<B: CpuBus>(cpu: &mut Cpu, bus: &mut B, comment: u8) -> bool {
    match comment {
        0x02 | 0x06 => swi_halt(cpu),
        0x03 => true,
        0x04 => swi_intr_wait(cpu),
        0x05 => swi_vblank_intr_wait(cpu),
        0x07 => {
            common::swi_div_arm(cpu);
            true
        }
        0x08 => {
            swi_change_sound_bias(cpu);
            true
        }
        0x09 => {
            common::swi_div(cpu);
            true
        }
        0x0B => {
            common::swi_cpu_set(cpu, bus);
            true
        }
        0x0C => {
            common::swi_cpu_fast_set(cpu, bus);
            true
        }
        0x0D => {
            common::swi_sqrt(cpu);
            true
        }
        0x0E => {
            common::swi_get_crc16(cpu, bus);
            true
        }
        0x0F => {
            swi_get_boot_procs(cpu);
            true
        }
        0x11 => {
            common::swi_lz77_uncomp(cpu, bus, false);
            true
        }
        0x12 => {
            common::swi_lz77_uncomp(cpu, bus, true);
            true
        }
        _ => {
            log::trace!("ARM9 unhandled SWI 0x{:02X}", comment);
            false
        }
    }
}

fn swi_change_sound_bias(_cpu: &mut Cpu) {}

fn swi_halt(cpu: &mut Cpu) -> bool {
    cpu.halted = true;
    true
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

fn swi_get_boot_procs(cpu: &mut Cpu) {
    // Modern libnds/calico calls ARM9 SWI 0x0F during MPU setup and uses R0
    // to choose the DS boot path. Direct boot without a real BIOS should
    // report the normal NDS path, not fall through to the exception vector.
    cpu.regs[0] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Arm9Memory;
    use crate::bus::{Bus9, SharedState};
    use crate::cpu::cp15::TcmRegion;

    #[test]
    fn test_swi_02_halts_cpu() {
        let mut cpu = Cpu::new_arm9();
        let mut mem = Arm9Memory::new(None);
        let mut shared = SharedState::new();
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );

        assert!(handle_swi(&mut cpu, &mut bus, 0x02));
        assert!(cpu.halted);
    }

    #[test]
    fn test_swi_03_wait_by_loop_returns() {
        let mut cpu = Cpu::new_arm9();
        cpu.halted = false;
        let mut mem = Arm9Memory::new(None);
        let mut shared = SharedState::new();
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );

        assert!(handle_swi(&mut cpu, &mut bus, 0x03));
        assert!(!cpu.halted);
    }

    #[test]
    fn test_swi_09_divide() {
        let mut cpu = Cpu::new_arm9();
        cpu.regs[0] = 100;
        cpu.regs[1] = 7;
        let mut mem = Arm9Memory::new(None);
        let mut shared = SharedState::new();
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );

        assert!(handle_swi(&mut cpu, &mut bus, 0x09));
        assert_eq!(cpu.regs[0], 14);
        assert_eq!(cpu.regs[1], 2);
    }

    #[test]
    fn test_swi_0f_reports_normal_ds_boot_path() {
        let mut cpu = Cpu::new_arm9();
        cpu.regs[0] = 0xDEAD_BEEF;
        let mut mem = Arm9Memory::new(None);
        let mut shared = SharedState::new();
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );

        assert!(handle_swi(&mut cpu, &mut bus, 0x0F));
        assert_eq!(cpu.regs[0], 0);
    }
}

//! ARM7 BIOS HLE — most SWIs are identical to ARM9; sound-driver SWIs
//! (0x1A-0x21) are stubbed and rely on a real BIOS for full coverage.

use super::common;
use crate::cpu::{Cpu, CpuBus};

pub fn handle_swi<B: CpuBus>(cpu: &mut Cpu, bus: &mut B, comment: u8) -> bool {
    match comment {
        0x02 | 0x06 => {
            swi_halt(cpu);
            true
        }
        0x03 => true,
        0x04 => {
            swi_intr_wait(cpu);
            true
        }
        0x05 => {
            swi_vblank_intr_wait(cpu);
            true
        }
        0x07 => {
            swi_sleep(cpu);
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
            swi_is_debugger(cpu);
            true
        }
        0x1F => {
            swi_set_halt_cr(cpu);
            true
        }
        0x20 => {
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
            log::trace!("ARM7 unhandled SWI 0x{:02X}", comment);
            false
        }
    }
}

fn swi_halt(cpu: &mut Cpu) {
    cpu.halted = true;
}

fn swi_sleep(cpu: &mut Cpu) {
    cpu.halted = true;
}

fn swi_change_sound_bias(_cpu: &mut Cpu) {}

fn swi_set_halt_cr(_cpu: &mut Cpu) {}

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

fn swi_get_boot_procs(cpu: &mut Cpu) {
    cpu.regs[0] = 0;
}

fn swi_is_debugger(cpu: &mut Cpu) {
    cpu.regs[0] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Arm7Memory, Bus7, SharedState};

    #[test]
    fn test_swi_02_halts_cpu() {
        let mut cpu = Cpu::new_arm7();
        let mut mem = Arm7Memory::new(None);
        let mut shared = SharedState::new();
        let mut bus = Bus7::new(&mut mem, &mut shared);

        assert!(handle_swi(&mut cpu, &mut bus, 0x02));
        assert!(cpu.halted);
    }

    #[test]
    fn test_swi_03_wait_by_loop_returns() {
        let mut cpu = Cpu::new_arm7();
        cpu.halted = false;
        let mut mem = Arm7Memory::new(None);
        let mut shared = SharedState::new();
        let mut bus = Bus7::new(&mut mem, &mut shared);

        assert!(handle_swi(&mut cpu, &mut bus, 0x03));
        assert!(!cpu.halted);
    }

    #[test]
    fn test_swi_07_sleep_halts_cpu() {
        let mut cpu = Cpu::new_arm7();
        let mut mem = Arm7Memory::new(None);
        let mut shared = SharedState::new();
        let mut bus = Bus7::new(&mut mem, &mut shared);

        assert!(handle_swi(&mut cpu, &mut bus, 0x07));
        assert!(cpu.halted);
    }

    #[test]
    fn test_swi_09_divide() {
        let mut cpu = Cpu::new_arm7();
        cpu.regs[0] = 100;
        cpu.regs[1] = 7;
        let mut mem = Arm7Memory::new(None);
        let mut shared = SharedState::new();
        let mut bus = Bus7::new(&mut mem, &mut shared);

        assert!(handle_swi(&mut cpu, &mut bus, 0x09));
        assert_eq!(cpu.regs[0], 14);
        assert_eq!(cpu.regs[1], 2);
    }

    #[test]
    fn test_swi_0f_reports_zero_status() {
        let mut cpu = Cpu::new_arm7();
        cpu.regs[0] = 0xDEAD_BEEF;
        let mut mem = Arm7Memory::new(None);
        let mut shared = SharedState::new();
        let mut bus = Bus7::new(&mut mem, &mut shared);

        assert!(handle_swi(&mut cpu, &mut bus, 0x0F));
        assert_eq!(cpu.regs[0], 0);
    }

}

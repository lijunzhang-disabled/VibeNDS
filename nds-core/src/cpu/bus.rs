//! `CpuBus` trait — what the CPU needs from its surrounding bus.
//!
//! The CPU is parameterized over this trait so the same `Cpu` struct can run
//! against either the ARM9 bus (`Bus9`) or the ARM7 bus (`Bus7`).

pub trait CpuBus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn read32(&mut self, addr: u32) -> u32;
    fn write8(&mut self, addr: u32, val: u8);
    fn write16(&mut self, addr: u32, val: u16);
    fn write32(&mut self, addr: u32, val: u32);

    /// True when an interrupt is pending and IRQ is enabled at the controller
    /// (CPSR I-bit is checked by the CPU separately).
    fn irq_pending(&self) -> bool {
        false
    }
}

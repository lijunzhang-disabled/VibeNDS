//! Per-CPU interrupt controllers.
//!
//! Each CPU has its own `IE`/`IF`/`IME` registers at `0x04000208`/
//! `0x04000210`/`0x04000214` (NDS uses 32-bit IE/IF, not 16-bit like GBA).
//! Many bits are CPU-specific:
//!
//! | Bit | ARM9 source | ARM7 source |
//! |---|---|---|
//! | 0 | VBlank | VBlank |
//! | 1 | HBlank | HBlank |
//! | 2 | VCount match | VCount match |
//! | 3 | Timer 0 | Timer 0 |
//! | 4 | Timer 1 | Timer 1 |
//! | 5 | Timer 2 | Timer 2 |
//! | 6 | Timer 3 | Timer 3 |
//! | 7 | (reserved on ARM9) | SIO/RCNT |
//! | 8 | DMA0 | DMA0 |
//! | 9 | DMA1 | DMA1 |
//! | 10 | DMA2 | DMA2 |
//! | 11 | DMA3 | DMA3 |
//! | 12 | Keypad | Keypad |
//! | 13 | GBA-slot | GBA-slot |
//! | 16 | IPC sync | IPC sync |
//! | 17 | IPC send FIFO empty | IPC send FIFO empty |
//! | 18 | IPC recv FIFO not-empty | IPC recv FIFO not-empty |
//! | 19 | NDS-slot data ready | NDS-slot data ready |
//! | 20 | NDS-slot card line | NDS-slot card line |
//! | 21 | GX FIFO | (reserved) |
//! | 22 | (reserved) | Lid open/close |
//! | 23 | (reserved) | SPI bus |
//! | 24 | (reserved) | WiFi |

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Irq {
    VBlank          = 0,
    HBlank          = 1,
    VCountMatch     = 2,
    Timer0          = 3,
    Timer1          = 4,
    Timer2          = 5,
    Timer3          = 6,
    Sio             = 7,   // ARM7 only
    Dma0            = 8,
    Dma1            = 9,
    Dma2            = 10,
    Dma3            = 11,
    Keypad          = 12,
    GbaSlot         = 13,
    IpcSync         = 16,
    IpcSendEmpty    = 17,
    IpcRecvNotEmpty = 18,
    Slot1Data       = 19,
    Slot1Card       = 20,
    GxFifo          = 21,  // ARM9 only
    LidOpen         = 22,  // ARM7 only
    Spi             = 23,  // ARM7 only
    WiFi            = 24,  // ARM7 only
}

impl Irq {
    #[inline]
    pub fn bit(self) -> u32 {
        1u32 << (self as u32)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptController {
    /// IE — Interrupt Enable. Bit n = source n is allowed to raise CPU IRQ.
    pub ie: u32,
    /// IF — Interrupt Request flags. Bit n = source n has fired and is
    /// pending acknowledgment. Writing 1 to a bit clears it.
    pub iflag: u32,
    /// IME — Interrupt Master Enable. When 0, the CPU is never interrupted
    /// regardless of IE/IF.
    pub ime: bool,
}

impl InterruptController {
    pub fn new() -> Self {
        InterruptController { ie: 0, iflag: 0, ime: false }
    }

    pub fn request(&mut self, irq: Irq) {
        self.iflag |= irq.bit();
    }

    pub fn raise_bits(&mut self, mask: u32) {
        self.iflag |= mask;
    }

    /// Acknowledge — write-1-to-clear semantics.
    pub fn acknowledge(&mut self, mask: u32) {
        self.iflag &= !mask;
    }

    pub fn has_pending(&self) -> bool {
        self.ime && (self.ie & self.iflag) != 0
    }

    pub fn read_ie(&self) -> u32 { self.ie }
    pub fn write_ie(&mut self, v: u32) { self.ie = v; }
    pub fn read_if(&self) -> u32 { self.iflag }
    pub fn write_if(&mut self, v: u32) { self.acknowledge(v); }
    pub fn read_ime(&self) -> u32 { self.ime as u32 }
    pub fn write_ime(&mut self, v: u32) { self.ime = v & 1 != 0; }
}

impl Default for InterruptController {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_and_pending() {
        let mut ic = InterruptController::new();
        ic.ime = true;
        ic.ie = Irq::VBlank.bit();
        assert!(!ic.has_pending());
        ic.request(Irq::VBlank);
        assert!(ic.has_pending());
        ic.write_if(Irq::VBlank.bit());
        assert!(!ic.has_pending());
    }

    #[test]
    fn test_ime_off_blocks_pending() {
        let mut ic = InterruptController::new();
        ic.ime = false;
        ic.ie = Irq::VBlank.bit();
        ic.request(Irq::VBlank);
        assert!(!ic.has_pending());
    }

    #[test]
    fn test_request_disabled_source_does_not_raise() {
        let mut ic = InterruptController::new();
        ic.ime = true;
        ic.ie = 0;
        ic.request(Irq::Dma0);
        assert!(!ic.has_pending());
    }

    #[test]
    fn test_ipc_recv_not_empty_bit_position() {
        // Bit 18 — used by every IPC-driven game. Worth nailing down.
        assert_eq!(Irq::IpcRecvNotEmpty.bit(), 1 << 18);
    }
}

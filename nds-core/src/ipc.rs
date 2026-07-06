//! Inter-Processor Communication: `IPCSYNC` (doorbell) + `IPCFIFO`.
//!
//! Concept doc: `docs/concepts/ipc.md`. Read that first if you're touching
//! this code — the per-CPU register-mirror semantics are non-obvious.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use crate::interrupt::{InterruptController, Irq};

/// Maximum FIFO depth (one direction).
pub const FIFO_DEPTH: usize = 16;

/// Which CPU is performing the operation. Used to disambiguate the
/// "this side / other side" of a single shared latch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Arm9,
    Arm7,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::Arm9 => Side::Arm7,
            Side::Arm7 => Side::Arm9,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ipc {
    // ─── IPCSYNC (0x04000180) ─────────────────────────────────────
    /// 4-bit "send" half — what the OTHER CPU sees in `recv_data`.
    pub sync_arm9_send: u8,
    pub sync_arm7_send: u8,
    /// Bit 14: enable receive IRQ on this CPU when the other CPU
    /// pulses bit 13.
    pub sync_arm9_recv_irq_en: bool,
    pub sync_arm7_recv_irq_en: bool,

    // ─── IPCFIFO (control + queues) ──────────────────────────────
    /// Per-CPU master enable (bit 15 of FIFOCNT).
    pub fifo_arm9_enable: bool,
    pub fifo_arm7_enable: bool,
    /// Send-FIFO-empty IRQ enable (bit 2 of FIFOCNT).
    pub fifo_arm9_send_empty_irq: bool,
    pub fifo_arm7_send_empty_irq: bool,
    /// Recv-FIFO-not-empty IRQ enable (bit 10 of FIFOCNT).
    pub fifo_arm9_recv_irq: bool,
    pub fifo_arm7_recv_irq: bool,
    /// Sticky error flag — set on read-empty or write-full, write-1 to clear.
    pub fifo_arm9_error: bool,
    pub fifo_arm7_error: bool,

    /// Outgoing FIFOs. `9to7` is what ARM9 writes via SEND and ARM7 reads via RECV.
    pub fifo_9to7: VecDeque<u32>,
    pub fifo_7to9: VecDeque<u32>,

    /// Last successfully popped word — returned on read-empty per real
    /// hardware behavior (see concept doc §10).
    pub last_popped_9: u32,
    pub last_popped_7: u32,
}

impl Ipc {
    pub fn new() -> Self {
        Self::default()
    }

    // ─── IPCSYNC ─────────────────────────────────────────────────

    /// Read IPCSYNC from `side`'s perspective.
    pub fn read_sync(&self, side: Side) -> u16 {
        let (mine, others, my_irq_en) = match side {
            Side::Arm9 => (
                self.sync_arm9_send,
                self.sync_arm7_send,
                self.sync_arm9_recv_irq_en,
            ),
            Side::Arm7 => (
                self.sync_arm7_send,
                self.sync_arm9_send,
                self.sync_arm7_recv_irq_en,
            ),
        };
        // bits[3:0] = recv_data (other CPU's send half),
        // bits[11:8] = my send half,
        // bit 14 = my receive IRQ enable.
        let mut v = (others as u16 & 0xF) | ((mine as u16 & 0xF) << 8);
        if my_irq_en {
            v |= 1 << 14;
        }
        v
    }

    /// Write IPCSYNC from `side`'s perspective. Returns true if a
    /// "send IRQ to other CPU" was triggered (bit 13 = 1 AND the OTHER
    /// CPU has its receive-IRQ-enable set), meaning the caller should
    /// raise `Irq::IpcSync` on the other CPU's controller.
    pub fn write_sync(&mut self, side: Side, val: u16) -> bool {
        let send_data = ((val >> 8) & 0xF) as u8;
        let my_recv_irq_en = val & (1 << 14) != 0;
        let trigger = val & (1 << 13) != 0;
        if std::env::var_os("NDS_TRACE_IPC").is_some() {
            eprintln!(
                "ipc sync {:?} send=0x{send_data:X} trigger={trigger} recv_irq_en={my_recv_irq_en}",
                side
            );
        }

        match side {
            Side::Arm9 => {
                self.sync_arm9_send = send_data;
                self.sync_arm9_recv_irq_en = my_recv_irq_en;
            }
            Side::Arm7 => {
                self.sync_arm7_send = send_data;
                self.sync_arm7_recv_irq_en = my_recv_irq_en;
            }
        }

        // Trigger raises the IPC-Sync IRQ on the OTHER CPU only if
        // they have their receive-IRQ-enable set.
        if trigger {
            let other_enable = match side.other() {
                Side::Arm9 => self.sync_arm9_recv_irq_en,
                Side::Arm7 => self.sync_arm7_recv_irq_en,
            };
            return other_enable;
        }
        false
    }

    // ─── IPCFIFOCNT ─────────────────────────────────────────────

    /// Read FIFOCNT from `side`'s perspective.
    pub fn read_fifocnt(&self, side: Side) -> u16 {
        let (enable, send_irq, recv_irq, error, send_q, recv_q) = match side {
            Side::Arm9 => (
                self.fifo_arm9_enable,
                self.fifo_arm9_send_empty_irq,
                self.fifo_arm9_recv_irq,
                self.fifo_arm9_error,
                &self.fifo_9to7, // ARM9's send queue
                &self.fifo_7to9, // ARM9's recv queue
            ),
            Side::Arm7 => (
                self.fifo_arm7_enable,
                self.fifo_arm7_send_empty_irq,
                self.fifo_arm7_recv_irq,
                self.fifo_arm7_error,
                &self.fifo_7to9,
                &self.fifo_9to7,
            ),
        };
        let mut v = 0u16;
        if send_q.is_empty() {
            v |= 1 << 0;
        }
        if send_q.len() == FIFO_DEPTH {
            v |= 1 << 1;
        }
        if send_irq {
            v |= 1 << 2;
        }
        if recv_q.is_empty() {
            v |= 1 << 8;
        }
        if recv_q.len() == FIFO_DEPTH {
            v |= 1 << 9;
        }
        if recv_irq {
            v |= 1 << 10;
        }
        if error {
            v |= 1 << 14;
        }
        if enable {
            v |= 1 << 15;
        }
        v
    }

    /// Write FIFOCNT from `side`. Returns a `FifoCntEffects` struct
    /// describing IRQs that need to be raised as a result.
    pub fn write_fifocnt(&mut self, side: Side, val: u16) -> FifoCntEffects {
        let mut effects = FifoCntEffects::default();

        // Bit 3 (when written 1) = clear this CPU's *send* FIFO.
        if val & (1 << 3) != 0 {
            match side {
                Side::Arm9 => self.fifo_9to7.clear(),
                Side::Arm7 => self.fifo_7to9.clear(),
            }
        }

        // Bit 14 = error. Write-1 clears it.
        let write_clear_error = val & (1 << 14) != 0;

        let new_send_irq_en = val & (1 << 2) != 0;
        let new_recv_irq_en = val & (1 << 10) != 0;
        let new_enable = val & (1 << 15) != 0;

        match side {
            Side::Arm9 => {
                let send_empty = self.fifo_9to7.is_empty();
                let was_recv_irq = self.fifo_arm9_recv_irq;
                let was_send_irq = self.fifo_arm9_send_empty_irq;
                self.fifo_arm9_send_empty_irq = new_send_irq_en;
                self.fifo_arm9_recv_irq = new_recv_irq_en;
                self.fifo_arm9_enable = new_enable;
                if write_clear_error {
                    self.fifo_arm9_error = false;
                }

                if !was_send_irq && new_send_irq_en && send_empty {
                    effects.raise_send_empty_on_self = true;
                }
                if !was_recv_irq && new_recv_irq_en && !self.fifo_7to9.is_empty() {
                    effects.raise_recv_not_empty_on_self = true;
                }
            }
            Side::Arm7 => {
                let send_empty = self.fifo_7to9.is_empty();
                let was_recv_irq = self.fifo_arm7_recv_irq;
                let was_send_irq = self.fifo_arm7_send_empty_irq;
                self.fifo_arm7_send_empty_irq = new_send_irq_en;
                self.fifo_arm7_recv_irq = new_recv_irq_en;
                self.fifo_arm7_enable = new_enable;
                if write_clear_error {
                    self.fifo_arm7_error = false;
                }

                if !was_send_irq && new_send_irq_en && send_empty {
                    effects.raise_send_empty_on_self = true;
                }
                if !was_recv_irq && new_recv_irq_en && !self.fifo_9to7.is_empty() {
                    effects.raise_recv_not_empty_on_self = true;
                }
            }
        }
        effects
    }

    // ─── IPCFIFOSEND (0x04000188) ───────────────────────────────

    /// Push a word into `side`'s send FIFO. Returns `Some(())` if the
    /// caller should raise `Irq::IpcRecvNotEmpty` on the OTHER CPU
    /// (empty→non-empty transition with the OTHER CPU's recv IRQ enabled).
    pub fn write_send(&mut self, side: Side, val: u32) -> bool {
        if std::env::var_os("NDS_TRACE_IPC").is_some() {
            eprintln!("ipc send {:?} val=0x{val:08X}", side);
        }
        let enable = match side {
            Side::Arm9 => self.fifo_arm9_enable,
            Side::Arm7 => self.fifo_arm7_enable,
        };
        if !enable {
            return false;
        }

        let queue = match side {
            Side::Arm9 => &mut self.fifo_9to7,
            Side::Arm7 => &mut self.fifo_7to9,
        };

        if queue.len() >= FIFO_DEPTH {
            // Overflow: drop, set sender's error flag.
            match side {
                Side::Arm9 => self.fifo_arm9_error = true,
                Side::Arm7 => self.fifo_arm7_error = true,
            }
            return false;
        }

        let was_empty = queue.is_empty();
        queue.push_back(val);

        // Empty→non-empty transition wakes the OTHER CPU.
        if was_empty {
            let other_recv_en = match side.other() {
                Side::Arm9 => self.fifo_arm9_recv_irq,
                Side::Arm7 => self.fifo_arm7_recv_irq,
            };
            return other_recv_en;
        }
        false
    }

    // ─── IPCFIFORECV (0x04100000) ───────────────────────────────

    /// Pop a word from `side`'s recv FIFO. Returns `(value, send_empty_on_other)`
    /// — the second is true if the caller should raise `Irq::IpcSendEmpty`
    /// on the OTHER CPU (their send queue just transitioned to empty).
    pub fn read_recv(&mut self, side: Side) -> (u32, bool) {
        let enable = match side {
            Side::Arm9 => self.fifo_arm9_enable,
            Side::Arm7 => self.fifo_arm7_enable,
        };
        if !enable {
            return (0, false);
        }

        let (queue, last_popped) = match side {
            Side::Arm9 => (&mut self.fifo_7to9, &mut self.last_popped_9),
            Side::Arm7 => (&mut self.fifo_9to7, &mut self.last_popped_7),
        };

        match queue.pop_front() {
            None => {
                // Empty: real hardware returns the last successfully popped
                // value and sets a sticky error flag on the reader.
                match side {
                    Side::Arm9 => self.fifo_arm9_error = true,
                    Side::Arm7 => self.fifo_arm7_error = true,
                }
                (*last_popped, false)
            }
            Some(val) => {
                *last_popped = val;
                let now_empty = queue.is_empty();
                if now_empty {
                    let other_send_en = match side.other() {
                        Side::Arm9 => self.fifo_arm9_send_empty_irq,
                        Side::Arm7 => self.fifo_arm7_send_empty_irq,
                    };
                    return (val, other_send_en);
                }
                (val, false)
            }
        }
    }
}

/// Effects of writing FIFOCNT — extra IRQs the I/O dispatcher must raise.
#[derive(Debug, Clone, Copy, Default)]
pub struct FifoCntEffects {
    pub raise_send_empty_on_self: bool,
    pub raise_recv_not_empty_on_self: bool,
}

/// Convenience: route IRQ raises to whichever CPU's `InterruptController`
/// the caller passes. Used by the I/O dispatchers in `bus/io_arm9.rs` and
/// `bus/io_arm7.rs`.
pub fn raise_ipc_sync(other_irq: &mut InterruptController) {
    other_irq.request(Irq::IpcSync);
}

pub fn raise_recv_not_empty(other_irq: &mut InterruptController) {
    other_irq.request(Irq::IpcRecvNotEmpty);
}

pub fn raise_send_empty(other_irq: &mut InterruptController) {
    other_irq.request(Irq::IpcSendEmpty);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_round_trip() {
        let mut ipc = Ipc::new();
        // ARM9 writes send=0xA, recv-irq-en=1
        ipc.write_sync(Side::Arm9, (0xA << 8) | (1 << 14));
        // ARM7 reads — should see 0xA in low nibble (recv_data)
        assert_eq!(ipc.read_sync(Side::Arm7) & 0xF, 0xA);
        // ARM9's read should see its own send in bits 11:8
        assert_eq!((ipc.read_sync(Side::Arm9) >> 8) & 0xF, 0xA);
        // ARM9's bit 14 should reflect the irq-enable
        assert!(ipc.read_sync(Side::Arm9) & (1 << 14) != 0);
    }

    #[test]
    fn test_sync_trigger_only_when_other_enabled() {
        let mut ipc = Ipc::new();
        // ARM7 has recv-irq-en NOT set → ARM9 trigger should not raise.
        let raised = ipc.write_sync(Side::Arm9, (0x5 << 8) | (1 << 13));
        assert!(!raised);

        // ARM7 enables recv-irq.
        ipc.write_sync(Side::Arm7, 1 << 14);

        // Now ARM9 trigger should report "raise on ARM7".
        let raised = ipc.write_sync(Side::Arm9, (0x5 << 8) | (1 << 13));
        assert!(raised);
    }

    #[test]
    fn test_sync_trigger_does_not_latch() {
        let mut ipc = Ipc::new();
        ipc.write_sync(Side::Arm7, 1 << 14);
        ipc.write_sync(Side::Arm9, (0x1 << 8) | (1 << 13));
        // Reading back, bit 13 should be 0.
        assert_eq!(ipc.read_sync(Side::Arm9) & (1 << 13), 0);
    }

    #[test]
    fn test_fifo_disabled_blocks_writes() {
        let mut ipc = Ipc::new();
        // No enable bit set
        ipc.write_send(Side::Arm9, 0xDEADBEEF);
        assert!(ipc.fifo_9to7.is_empty());
    }

    fn enable_both(ipc: &mut Ipc) {
        ipc.write_fifocnt(Side::Arm9, 1 << 15);
        ipc.write_fifocnt(Side::Arm7, 1 << 15);
    }

    #[test]
    fn test_fifo_push_and_pop() {
        let mut ipc = Ipc::new();
        enable_both(&mut ipc);
        ipc.write_send(Side::Arm9, 0x1111_2222);
        let (val, _) = ipc.read_recv(Side::Arm7);
        assert_eq!(val, 0x1111_2222);
        // Now the queue should be empty
        let (val, _) = ipc.read_recv(Side::Arm7);
        // Empty read returns last popped value
        assert_eq!(val, 0x1111_2222);
        assert!(ipc.fifo_arm7_error);
    }

    #[test]
    fn test_fifo_overflow_sets_error_and_drops_write() {
        let mut ipc = Ipc::new();
        enable_both(&mut ipc);
        for i in 0..FIFO_DEPTH as u32 {
            ipc.write_send(Side::Arm9, i);
        }
        assert_eq!(ipc.fifo_9to7.len(), FIFO_DEPTH);
        // 17th push: should drop and set error.
        ipc.write_send(Side::Arm9, 0xDEAD);
        assert_eq!(ipc.fifo_9to7.len(), FIFO_DEPTH);
        assert!(ipc.fifo_arm9_error);
    }

    #[test]
    fn test_recv_not_empty_irq_only_on_transition() {
        let mut ipc = Ipc::new();
        enable_both(&mut ipc);
        // ARM7 enables recv IRQ
        ipc.write_fifocnt(Side::Arm7, (1 << 15) | (1 << 10));

        // First push: ARM7 was empty → IRQ should raise.
        let raised1 = ipc.write_send(Side::Arm9, 1);
        assert!(raised1);

        // Second push: ARM7 already non-empty → no new IRQ.
        let raised2 = ipc.write_send(Side::Arm9, 2);
        assert!(!raised2);

        // Drain.
        ipc.read_recv(Side::Arm7);
        ipc.read_recv(Side::Arm7);

        // Push again: empty→non-empty again → IRQ.
        let raised3 = ipc.write_send(Side::Arm9, 3);
        assert!(raised3);
    }

    #[test]
    fn test_send_empty_irq_only_on_drain_to_empty() {
        let mut ipc = Ipc::new();
        enable_both(&mut ipc);
        // ARM9 enables send-empty IRQ; pushes two words.
        ipc.write_fifocnt(Side::Arm9, (1 << 15) | (1 << 2));
        ipc.write_send(Side::Arm9, 1);
        ipc.write_send(Side::Arm9, 2);

        // ARM7 reads first — queue still non-empty → no send-empty IRQ.
        let (_, raised1) = ipc.read_recv(Side::Arm7);
        assert!(!raised1);

        // ARM7 reads second — drains to empty → ARM9 should be raised.
        let (_, raised2) = ipc.read_recv(Side::Arm7);
        assert!(raised2);
    }

    #[test]
    fn test_enabling_send_empty_irq_raises_if_fifo_already_empty() {
        let mut ipc = Ipc::new();
        enable_both(&mut ipc);

        let effects = ipc.write_fifocnt(Side::Arm9, (1 << 15) | (1 << 2));

        assert!(effects.raise_send_empty_on_self);
    }

    #[test]
    fn test_fifocnt_status_bits() {
        let mut ipc = Ipc::new();
        enable_both(&mut ipc);

        let cnt = ipc.read_fifocnt(Side::Arm9);
        assert!(cnt & (1 << 0) != 0, "send empty");
        assert!(cnt & (1 << 8) != 0, "recv empty");
        assert!(cnt & (1 << 15) != 0, "enable");

        ipc.write_send(Side::Arm9, 0xAA);
        let cnt = ipc.read_fifocnt(Side::Arm9);
        assert!(cnt & (1 << 0) == 0, "send no longer empty");

        let cnt7 = ipc.read_fifocnt(Side::Arm7);
        assert!(cnt7 & (1 << 8) == 0, "ARM7 sees recv non-empty");
    }

    #[test]
    fn test_clear_send_fifo_via_bit3() {
        let mut ipc = Ipc::new();
        enable_both(&mut ipc);
        for i in 0..5 {
            ipc.write_send(Side::Arm9, i);
        }
        assert_eq!(ipc.fifo_9to7.len(), 5);
        // ARM9 writes bit 3 set → clears its OWN send FIFO.
        ipc.write_fifocnt(Side::Arm9, (1 << 15) | (1 << 3));
        assert!(ipc.fifo_9to7.is_empty());
    }

    #[test]
    fn test_error_flag_cleared_by_writing_one() {
        let mut ipc = Ipc::new();
        enable_both(&mut ipc);
        // Trigger an error by reading empty.
        ipc.read_recv(Side::Arm9);
        assert!(ipc.fifo_arm9_error);
        // Write FIFOCNT with bit 14 set → clears.
        ipc.write_fifocnt(Side::Arm9, (1 << 15) | (1 << 14));
        assert!(!ipc.fifo_arm9_error);
    }

    #[test]
    fn test_in_order_delivery() {
        let mut ipc = Ipc::new();
        enable_both(&mut ipc);
        for i in 0..4 {
            ipc.write_send(Side::Arm9, i + 100);
        }
        for i in 0..4 {
            let (v, _) = ipc.read_recv(Side::Arm7);
            assert_eq!(v, i + 100);
        }
    }
}

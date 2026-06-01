//! GXFIFO — 256-entry × 32-bit command FIFO.
//!
//! Sits at `0x04000400` (packed format) and `0x04000440..0x040005FF`
//! (direct ports). Both paths produce the same stream of `GxOp` entries
//! that the engine dispatcher consumes.
//!
//! ## Packed format (`0x04000400`)
//!
//! One 32-bit word `0x WW XX YY ZZ` packs up to 4 command IDs (one byte
//! each, LSB-first: `ZZ`, `YY`, `XX`, `WW`). After the packed word, the
//! ARM9 writes the parameters for each command in declaration order:
//! all of cmd1's params, then all of cmd2's params, etc. Commands with
//! zero parameters (like `MTX_PUSH`) take no follow-up words.
//!
//! ## Direct format (`0x04000440..0x040005FF`)
//!
//! Each address corresponds to one specific command. The word written
//! there is the (first) parameter; multi-parameter commands continue at
//! the same address with subsequent writes.
//!
//! The FIFO itself is bounded to 256 entries; writes past full drop and
//! set `GXSTAT.list_overflow`. Reading the FIFO is unusual on real
//! hardware (it's primarily write-only from software's view); we don't
//! expose reads.
//!
//! Output: a queue of `GxOp { cmd, params }`. The engine pops from this
//! queue and dispatches; the FIFO is just the producer.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use super::command::GxCmd;

/// Maximum entries the FIFO holds (packed + parameter words combined).
pub const FIFO_CAPACITY: usize = 256;
/// Half-full threshold — when the FIFO drops below this, GXFIFO DMA fires.
pub const FIFO_HALF: usize = 128;

/// One decoded command + its parameter words, ready for the dispatcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GxOp {
    pub cmd: u8,
    pub params: Vec<u32>,
}

/// FIFO + packed-word decoder + direct-port accumulator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GxFifo {
    /// Raw 32-bit words queued. We store words rather than decoded ops
    /// because we need to count raw words for the half-full DMA threshold.
    pub words: VecDeque<u32>,

    /// Decoder state for the packed (`0x04000400`) write path.
    /// When a packed word arrives we extract up to 4 command IDs and
    /// remember how many params each is still owed. As parameter words
    /// follow, we attach them; once a command's param count is satisfied
    /// it gets emitted as a `GxOp`.
    pending_cmds: VecDeque<PackedCmd>,

    /// Decoded ops ready for the dispatcher to consume.
    pub ready: VecDeque<GxOp>,

    /// Sticky overflow flag (`GXSTAT.list_overflow`).
    pub overflow: bool,

    /// Set after each accept; the bus dispatcher reads this to decide
    /// whether to fire the GxFifo DMA trigger.
    pub fell_below_half: bool,

    /// GXSTAT bits 30-31: 0=never, 1=less-than-half, 2=empty.
    pub irq_mode: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackedCmd {
    cmd: u8,
    remaining: u8,
    params: Vec<u32>,
}

impl GxFifo {
    pub fn new() -> Self {
        GxFifo {
            words: VecDeque::with_capacity(FIFO_CAPACITY),
            pending_cmds: VecDeque::new(),
            ready: VecDeque::new(),
            overflow: false,
            fell_below_half: false,
            irq_mode: 0,
        }
    }

    pub fn is_empty(&self) -> bool { self.words.is_empty() }
    pub fn is_full(&self)  -> bool { self.words.len() >= FIFO_CAPACITY }
    pub fn len(&self)      -> usize { self.words.len() }

    /// Write to the packed-format port at `0x04000400`. Pushes one word
    /// into the FIFO and decodes commands as parameter words accumulate.
    pub fn write_packed(&mut self, word: u32) {
        if self.is_full() {
            self.overflow = true;
            return;
        }
        let prev_len = self.words.len();
        self.words.push_back(word);

        // If no commands are currently waiting on parameters, this word
        // is itself a packed-command word: extract 4 command IDs.
        if self.pending_cmds.is_empty() {
            self.unpack_packed_word(word);
        } else {
            // Otherwise this word is a parameter for the front pending cmd.
            self.consume_param(word);
        }

        self.maybe_emit_zero_param_cmds();

        // Track the half-full transition for the DMA dispatcher.
        if prev_len >= FIFO_HALF && self.words.len() < FIFO_HALF {
            // This branch never fires on push (we add, not remove). But we
            // capture half-full-after-drain in `pop_op` below.
            self.fell_below_half = true;
        }
    }

    /// Write to a direct port. `cmd` is the GX opcode the address maps to.
    /// First write supplies parameter 1; subsequent writes to the same
    /// command continue the parameter sequence until satisfied.
    pub fn write_direct(&mut self, cmd: GxCmd, word: u32) {
        if self.is_full() {
            self.overflow = true;
            return;
        }
        self.words.push_back(word);

        // Direct-port writes are a 1-command equivalent. If the same
        // command appeared in `pending_cmds`, append; otherwise push a
        // fresh entry.
        let cmd_byte = cmd as u8;
        let needed = cmd.param_count();

        if needed == 0 {
            self.ready.push_back(GxOp { cmd: cmd_byte, params: Vec::new() });
            return;
        }

        if let Some(front) = self.pending_cmds.back_mut() {
            if front.cmd == cmd_byte && (front.params.len() as u8) < front.remaining {
                front.params.push(word);
                if front.params.len() as u8 == front.remaining {
                    let done = self.pending_cmds.pop_back().unwrap();
                    self.ready.push_back(GxOp { cmd: done.cmd, params: done.params });
                }
                return;
            }
        }

        // New command — push and accumulate.
        if needed == 1 {
            self.ready.push_back(GxOp { cmd: cmd_byte, params: vec![word] });
        } else {
            self.pending_cmds.push_back(PackedCmd {
                cmd: cmd_byte,
                remaining: needed,
                params: vec![word],
            });
        }
    }

    fn unpack_packed_word(&mut self, word: u32) {
        // Four command IDs, LSB-first.
        for shift in [0u32, 8, 16, 24] {
            let id = ((word >> shift) & 0xFF) as u8;
            if id == 0 { continue; } // padding — skip
            if let Some(cmd) = GxCmd::from_u8(id) {
                let needed = cmd.param_count();
                self.pending_cmds.push_back(PackedCmd {
                    cmd: id,
                    remaining: needed,
                    params: Vec::with_capacity(needed as usize),
                });
            } else {
                log::trace!("GXFIFO: unknown cmd byte 0x{:02X}", id);
            }
        }
    }

    fn consume_param(&mut self, word: u32) {
        if let Some(front) = self.pending_cmds.front_mut() {
            front.params.push(word);
            if front.params.len() as u8 == front.remaining {
                let done = self.pending_cmds.pop_front().unwrap();
                self.ready.push_back(GxOp { cmd: done.cmd, params: done.params });
            }
        }
    }

    /// Emit any pending commands that need zero parameters (e.g. MTX_PUSH).
    fn maybe_emit_zero_param_cmds(&mut self) {
        while let Some(front) = self.pending_cmds.front() {
            if front.remaining == 0 {
                let done = self.pending_cmds.pop_front().unwrap();
                self.ready.push_back(GxOp { cmd: done.cmd, params: done.params });
            } else {
                break;
            }
        }
    }

    /// Pop one ready op for the dispatcher. Also consumes the word(s) in
    /// the raw `words` queue corresponding to this op (1 packed-word
    /// header byte's worth + `param_count` parameter words). When the
    /// queue crosses below the half-full mark, set `fell_below_half` so
    /// the caller can fire GxFifo DMA.
    pub fn pop_op(&mut self) -> Option<GxOp> {
        let op = self.ready.pop_front()?;
        // Drop `1 + params.len()` words (header byte share + parameter words).
        // The header-byte share is fractional (4 cmds share one word), so we
        // approximate by dropping `params.len()` actual words and counting
        // 1 word "spent" every 4 commands processed. For DMA accounting we
        // overcount slightly toward "free" rather than miss a refill.
        let to_drop = op.params.len().max(1);
        for _ in 0..to_drop {
            if self.words.pop_front().is_none() { break; }
        }
        if self.words.len() < FIFO_HALF {
            self.fell_below_half = true;
        }
        Some(op)
    }

    /// Take the "fell below half" edge flag (caller clears it).
    pub fn take_below_half_edge(&mut self) -> bool {
        let v = self.fell_below_half;
        self.fell_below_half = false;
        v
    }

    pub fn reconcile_after_drain(&mut self) {
        if self.ready.is_empty() && self.pending_cmds.is_empty() && !self.words.is_empty() {
            self.words.clear();
            self.fell_below_half = true;
        }
    }

    /// Build the `GXSTAT` register value (low 16 bits — the high half
    /// holds command-list-size and similar fields managed elsewhere).
    pub fn stat_low(&self) -> u16 {
        let mut v = 0u16;
        if self.words.is_empty() { v |= 1 << 0; }           // FIFO empty
        if self.is_full() { v |= 1 << 1; }                  // FIFO full
        if self.words.len() < FIFO_HALF { v |= 1 << 2; }    // less than half full
        // Bit 3 = command-list-overflow (also reported via overflow flag)
        if self.overflow { v |= 1 << 15; }
        v
    }

    pub fn stat_high(&self) -> u16 {
        let count = self.words.len().min(256) as u16;
        let mut v = count;
        if self.is_full() { v |= 1 << 8; }                  // GXSTAT bit 24
        if self.words.len() < FIFO_HALF { v |= 1 << 9; }    // GXSTAT bit 25
        if self.words.is_empty() { v |= 1 << 10; }          // GXSTAT bit 26
        v |= (self.irq_mode as u16) << 14;                  // GXSTAT bits 30-31
        v
    }

    pub fn set_irq_mode(&mut self, mode: u8) {
        self.irq_mode = mode & 0x3;
    }

    pub fn irq_condition(&self) -> bool {
        match self.irq_mode {
            1 => self.words.len() < FIFO_HALF,
            2 => self.words.is_empty(),
            _ => false,
        }
    }
}

impl Default for GxFifo {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packed_word_with_one_zero_param_command() {
        let mut f = GxFifo::new();
        // MTX_PUSH (0x11) packed alone — zero params.
        f.write_packed(0x0000_0011);
        let op = f.pop_op().expect("op");
        assert_eq!(op.cmd, 0x11);
        assert!(op.params.is_empty());
    }

    #[test]
    fn test_packed_word_with_one_param_command() {
        let mut f = GxFifo::new();
        // MTX_MODE (0x10), one param = 1 (= MtxMode::Position).
        f.write_packed(0x0000_0010);
        f.write_packed(1);
        let op = f.pop_op().expect("op");
        assert_eq!(op.cmd, 0x10);
        assert_eq!(op.params, vec![1]);
    }

    #[test]
    fn test_packed_word_with_four_commands_and_params() {
        let mut f = GxFifo::new();
        // Pack: MTX_PUSH (0), MTX_MODE (1), MTX_IDENTITY (0), MTX_POP (1).
        // Encoding LSB-first: 0x12_15_10_11 = 0x12151011.
        f.write_packed(0x1215_1011);
        // Parameters in declaration order: MTX_MODE gets 1, MTX_POP gets 5.
        f.write_packed(2);
        f.write_packed(5);

        let ops: Vec<_> = std::iter::from_fn(|| f.pop_op()).collect();
        assert_eq!(ops.len(), 4);
        assert_eq!(ops[0].cmd, 0x11); assert!(ops[0].params.is_empty());
        assert_eq!(ops[1].cmd, 0x10); assert_eq!(ops[1].params, vec![2]);
        assert_eq!(ops[2].cmd, 0x15); assert!(ops[2].params.is_empty());
        assert_eq!(ops[3].cmd, 0x12); assert_eq!(ops[3].params, vec![5]);
    }

    #[test]
    fn test_direct_port_writes() {
        let mut f = GxFifo::new();
        // VTX_16 needs 2 params.
        f.write_direct(GxCmd::Vtx16, 0xAAAA_AAAA);
        // Not yet ready — needs 1 more.
        assert!(f.ready.is_empty());
        f.write_direct(GxCmd::Vtx16, 0xBBBB_BBBB);
        let op = f.pop_op().expect("op");
        assert_eq!(op.cmd, 0x23);
        assert_eq!(op.params, vec![0xAAAA_AAAA, 0xBBBB_BBBB]);
    }

    #[test]
    fn test_full_then_overflow() {
        let mut f = GxFifo::new();
        for _ in 0..FIFO_CAPACITY {
            f.write_packed(0); // padding word
        }
        assert!(f.is_full());
        assert!(!f.overflow);
        f.write_packed(0xDEADBEEF);
        assert!(f.overflow);
    }

    #[test]
    fn test_below_half_edge_after_drain() {
        let mut f = GxFifo::new();
        // Push 200 op-pairs that each consume 1 word; drop the half-flag
        // first since fill doesn't set it.
        for _ in 0..200 {
            f.write_packed(0x0000_0011); // MTX_PUSH, zero params
        }
        let _ = f.take_below_half_edge(); // discard whatever push set
        // Drain back below 128.
        for _ in 0..80 {
            let _ = f.pop_op();
        }
        assert!(f.take_below_half_edge(), "should signal below-half on drain");
    }

    #[test]
    fn test_stat_low_bits_for_empty_fifo() {
        let f = GxFifo::new();
        let s = f.stat_low();
        assert!(s & (1 << 0) != 0, "empty");
        assert!(s & (1 << 2) != 0, "less than half");
        assert!(s & (1 << 1) == 0, "not full");
    }
}

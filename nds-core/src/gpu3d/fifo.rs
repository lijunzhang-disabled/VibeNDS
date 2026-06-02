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
//! zero parameters (like `MTX_PUSH`) do not emit normal params, but a
//! zero-param final real command still consumes the hardware-required dummy
//! word before the next packed command word.
//!
//! ## Direct format (`0x04000440..0x040005FF`)
//!
//! Each address corresponds to one specific command. The word written
//! there is the (first) parameter; multi-parameter commands continue at
//! the same address with subsequent writes.
//!
//! The hardware FIFO itself is bounded to 256 entries, but CPU writes stall
//! when it is full instead of dropping command data. This emulator does not
//! model the stall timing yet, so it preserves over-capacity writes in order
//! rather than corrupting the command stream. Reading the FIFO is unusual on
//! real hardware (it's primarily write-only from software's view); we don't
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

    /// Number of 40-bit FIFO entries occupied by decoded commands. Commands
    /// without parameters count as one entry; commands with N parameters
    /// count as N entries.
    pub entries: usize,

    /// Decoder state for the packed (`0x04000400`) write path.
    /// When a packed word arrives we extract up to 4 command IDs and
    /// remember how many params each is still owed. As parameter words
    /// follow, we attach them; once a command's param count is satisfied
    /// it gets emitted as a `GxOp`.
    pending_cmds: VecDeque<PackedCmd>,

    /// Decoder state for direct-port writes. Multi-parameter direct commands
    /// accumulate only with following writes to the same direct command port;
    /// they must not consume pending packed-command parameters.
    direct_pending: Option<PackedCmd>,

    /// Decoded ops ready for the dispatcher to consume.
    pub ready: VecDeque<GxOp>,

    /// Sticky internal overflow flag used by geometry-buffer limit tests.
    /// `GXSTAT` bit 15 is the matrix stack overflow/underflow flag, not a
    /// FIFO overflow flag.
    pub overflow: bool,

    /// Set after each accept; the bus dispatcher reads this to decide
    /// whether to fire the GxFifo DMA trigger.
    pub fell_below_half: bool,

    /// GXSTAT bits 30-31: 0=never, 1=less-than-half, 2=empty.
    pub irq_mode: u8,

    /// Packed FIFO rule: if the command word's final command takes no
    /// parameters, the next word is a dummy parameter before another command
    /// word may be accepted.
    needs_zero_param_tail_dummy: bool,
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
            entries: 0,
            pending_cmds: VecDeque::new(),
            direct_pending: None,
            ready: VecDeque::new(),
            overflow: false,
            fell_below_half: false,
            irq_mode: 0,
            needs_zero_param_tail_dummy: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
    pub fn is_full(&self) -> bool {
        self.entries >= FIFO_CAPACITY
    }
    pub fn len(&self) -> usize {
        self.entries
    }

    /// Write to the packed-format port at `0x04000400`. Pushes one word
    /// into the FIFO and decodes commands as parameter words accumulate.
    pub fn write_packed(&mut self, word: u32) {
        let prev_len = self.entries;
        self.words.push_back(word);

        if !self.pending_cmds.is_empty() {
            // Otherwise this word is a parameter for the front pending cmd.
            self.consume_param(word);
        } else if self.needs_zero_param_tail_dummy {
            self.needs_zero_param_tail_dummy = false;
        } else {
            // If no commands are currently waiting on parameters, this word
            // is itself a packed-command word: extract 4 command IDs.
            self.unpack_packed_word(word);
        }

        self.maybe_emit_zero_param_cmds();

        // Track the half-full transition for the DMA dispatcher.
        if prev_len >= FIFO_HALF && self.entries < FIFO_HALF {
            // This branch never fires on push (we add, not remove). But we
            // capture half-full-after-drain in `pop_op` below.
            self.fell_below_half = true;
        }
    }

    /// Write to a direct port. `cmd` is the GX opcode the address maps to.
    /// First write supplies parameter 1; subsequent writes to the same
    /// command continue the parameter sequence until satisfied.
    pub fn write_direct(&mut self, cmd: GxCmd, word: u32) {
        self.words.push_back(word);

        let cmd_byte = cmd as u8;
        let needed = cmd.param_count();

        if needed == 0 {
            self.entries += 1;
            self.ready.push_back(GxOp {
                cmd: cmd_byte,
                params: Vec::new(),
            });
            return;
        }

        if let Some(front) = self.direct_pending.as_mut() {
            if front.cmd == cmd_byte && (front.params.len() as u8) < front.remaining {
                self.entries += 1;
                front.params.push(word);
                if front.params.len() as u8 == front.remaining {
                    let done = self.direct_pending.take().unwrap();
                    self.ready.push_back(GxOp {
                        cmd: done.cmd,
                        params: done.params,
                    });
                }
                return;
            }
        }

        // New command — push and accumulate.
        if needed == 1 {
            self.entries += 1;
            self.ready.push_back(GxOp {
                cmd: cmd_byte,
                params: vec![word],
            });
        } else {
            self.entries += 1;
            self.direct_pending = Some(PackedCmd {
                cmd: cmd_byte,
                remaining: needed,
                params: vec![word],
            });
        }
    }

    fn unpack_packed_word(&mut self, word: u32) {
        // Four command IDs, LSB-first.
        let mut saw_command = false;
        let mut last_remaining = 0;
        for shift in [0u32, 8, 16, 24] {
            let id = ((word >> shift) & 0xFF) as u8;
            if id == 0 {
                // Command 0 terminates the packed command list. Hardware
                // requires zero padding only after all real commands; do not
                // accept later non-zero bytes from malformed command words.
                break;
            }
            let Some(cmd) = GxCmd::from_u8(id) else {
                // Invalid command indices behave like command 0, so they
                // terminate the packed command list.
                log::trace!("GXFIFO: unknown cmd byte 0x{:02X}", id);
                break;
            };
            let needed = cmd.param_count();
            saw_command = true;
            last_remaining = needed;
            self.pending_cmds.push_back(PackedCmd {
                cmd: id,
                remaining: needed,
                params: Vec::with_capacity(needed as usize),
            });
        }
        self.needs_zero_param_tail_dummy = saw_command && last_remaining == 0;
    }

    fn consume_param(&mut self, word: u32) {
        if let Some(front) = self.pending_cmds.front_mut() {
            self.entries += 1;
            front.params.push(word);
            if front.params.len() as u8 == front.remaining {
                let done = self.pending_cmds.pop_front().unwrap();
                self.ready.push_back(GxOp {
                    cmd: done.cmd,
                    params: done.params,
                });
            }
        }
    }

    /// Emit any pending commands that need zero parameters (e.g. MTX_PUSH).
    fn maybe_emit_zero_param_cmds(&mut self) {
        while let Some(front) = self.pending_cmds.front() {
            if front.remaining == 0 {
                let done = self.pending_cmds.pop_front().unwrap();
                self.entries += 1;
                self.ready.push_back(GxOp {
                    cmd: done.cmd,
                    params: done.params,
                });
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
        let consumed_entries = op.params.len().max(1);
        self.entries = self.entries.saturating_sub(consumed_entries);
        // Drop `1 + params.len()` words (header byte share + parameter words).
        // The header-byte share is fractional (4 cmds share one word), so we
        // approximate by dropping `params.len()` actual words and counting
        // 1 word "spent" every 4 commands processed. For DMA accounting we
        // overcount slightly toward "free" rather than miss a refill.
        let to_drop = op.params.len().max(1);
        for _ in 0..to_drop {
            if self.words.pop_front().is_none() {
                break;
            }
        }
        if self.entries < FIFO_HALF {
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
        if self.ready.is_empty()
            && self.pending_cmds.is_empty()
            && self.direct_pending.is_none()
            && !self.words.is_empty()
        {
            self.words.clear();
            self.entries = 0;
            self.fell_below_half = true;
        }
    }

    pub fn stat_high(&self) -> u16 {
        let count = self.entries.min(256) as u16;
        let mut v = count;
        if self.is_full() {
            v |= 1 << 8;
        } // GXSTAT bit 24
        if self.entries < FIFO_HALF {
            v |= 1 << 9;
        } // GXSTAT bit 25
        if self.entries == 0 {
            v |= 1 << 10;
        } // GXSTAT bit 26
        v |= (self.irq_mode as u16) << 14; // GXSTAT bits 30-31
        v
    }

    pub fn set_irq_mode(&mut self, mode: u8) {
        self.irq_mode = mode & 0x3;
    }

    pub fn irq_condition(&self) -> bool {
        match self.irq_mode {
            1 => self.entries < FIFO_HALF,
            2 => self.entries == 0,
            _ => false,
        }
    }

    pub fn gxstat_high_bits(&self, general_busy: bool) -> u16 {
        let count = self.entries.min(256) as u16;
        let mut v = count;
        if self.entries < FIFO_HALF {
            v |= 1 << 9;
        } // GXSTAT bit 25
        if self.entries == 0 {
            v |= 1 << 10;
        } // GXSTAT bit 26
        if general_busy {
            v |= 1 << 11;
        } // GXSTAT bit 27
        v |= (self.irq_mode as u16) << 14; // GXSTAT bits 30-31
        v
    }
}

impl Default for GxFifo {
    fn default() -> Self {
        Self::new()
    }
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
        assert_eq!(ops[0].cmd, 0x11);
        assert!(ops[0].params.is_empty());
        assert_eq!(ops[1].cmd, 0x10);
        assert_eq!(ops[1].params, vec![2]);
        assert_eq!(ops[2].cmd, 0x15);
        assert!(ops[2].params.is_empty());
        assert_eq!(ops[3].cmd, 0x12);
        assert_eq!(ops[3].params, vec![5]);
    }

    #[test]
    fn test_packed_zero_param_commands_count_as_fifo_entries() {
        let mut f = GxFifo::new();
        f.write_packed(0x1515_1515);

        assert_eq!(f.len(), 4);
        assert_eq!(f.gxstat_high_bits(false) & 0x01FF, 4);

        let ops: Vec<_> = std::iter::from_fn(|| f.pop_op()).collect();
        assert_eq!(ops.len(), 4);
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn test_packed_command_word_past_capacity_preserves_commands() {
        let mut f = GxFifo::new();
        f.entries = FIFO_CAPACITY - 1;

        f.write_packed(0x1515_1515);

        assert!(!f.overflow);
        assert_eq!(f.len(), FIFO_CAPACITY + 3);
        assert_eq!(f.gxstat_high_bits(false) & 0x01FF, 256);
        let ops: Vec<_> = std::iter::from_fn(|| f.pop_op()).collect();
        assert_eq!(ops.len(), 4);
        assert!(ops.iter().all(|op| op.cmd == GxCmd::MtxIdentity as u8));
    }

    #[test]
    fn test_packed_word_stops_at_first_zero_command_byte() {
        let mut f = GxFifo::new();
        // MTX_PUSH, then zero padding, then malformed non-zero bytes that
        // must not be accepted as commands.
        f.write_packed(0x1215_0011);

        let ops: Vec<_> = std::iter::from_fn(|| f.pop_op()).collect();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].cmd, 0x11);
        assert!(ops[0].params.is_empty());
    }

    #[test]
    fn test_packed_word_invalid_command_byte_acts_like_zero() {
        let mut f = GxFifo::new();
        // MTX_PUSH, invalid byte 0xFF, then valid bytes that must be ignored.
        f.write_packed(0x1215_FF11);

        let ops: Vec<_> = std::iter::from_fn(|| f.pop_op()).collect();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].cmd, 0x11);
        assert!(ops[0].params.is_empty());
    }

    #[test]
    fn test_zero_padded_packed_word_ending_with_zero_param_command_requires_dummy() {
        let mut f = GxFifo::new();
        f.write_packed(0x0000_0011); // MTX_PUSH, zero params, then zero padding.
        f.write_packed(0x0000_0010); // Required dummy, not MTX_MODE command word.
        f.write_packed(0x0000_0010); // Actual MTX_MODE command word.
        f.write_packed(1);

        let ops: Vec<_> = std::iter::from_fn(|| f.pop_op()).collect();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].cmd, 0x11);
        assert_eq!(ops[1].cmd, 0x10);
        assert_eq!(ops[1].params, vec![1]);
    }

    #[test]
    fn test_packed_word_tail_dummy_waits_until_prior_params_consumed() {
        let mut f = GxFifo::new();
        // MTX_MODE needs one parameter, then MTX_PUSH is the final command and
        // must consume a dummy before the next command word.
        f.write_packed(0x0000_1110);
        f.write_packed(2); // MTX_MODE parameter.
        f.write_packed(0x0000_0012); // Required dummy, not MTX_POP.
        f.write_packed(0x0000_0012); // Actual MTX_POP command word.
        f.write_packed(1);

        let ops: Vec<_> = std::iter::from_fn(|| f.pop_op()).collect();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].cmd, 0x10);
        assert_eq!(ops[0].params, vec![2]);
        assert_eq!(ops[1].cmd, 0x11);
        assert_eq!(ops[2].cmd, 0x12);
        assert_eq!(ops[2].params, vec![1]);
    }

    #[test]
    fn test_full_packed_word_ending_with_zero_param_command_requires_dummy_word() {
        let mut f = GxFifo::new();
        f.write_packed(0x1111_1111); // Four MTX_PUSH commands, no zero padding.
        f.write_packed(0x0000_0010); // Required dummy, not MTX_MODE command word.
        f.write_packed(0x0000_0010); // Actual MTX_MODE command word.
        f.write_packed(1);

        let ops: Vec<_> = std::iter::from_fn(|| f.pop_op()).collect();
        assert_eq!(ops.len(), 5);
        assert!(ops[..4]
            .iter()
            .all(|op| op.cmd == 0x11 && op.params.is_empty()));
        assert_eq!(ops[4].cmd, 0x10);
        assert_eq!(ops[4].params, vec![1]);
    }

    #[test]
    fn test_direct_port_writes() {
        let mut f = GxFifo::new();
        // VTX_16 needs 2 params.
        f.write_direct(GxCmd::Vtx16, 0xAAAA_AAAA);
        // Not yet ready — needs 1 more.
        assert!(f.ready.is_empty());
        assert_eq!(f.len(), 1);
        f.write_direct(GxCmd::Vtx16, 0xBBBB_BBBB);
        assert_eq!(f.len(), 2);
        let op = f.pop_op().expect("op");
        assert_eq!(op.cmd, 0x23);
        assert_eq!(op.params, vec![0xAAAA_AAAA, 0xBBBB_BBBB]);
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn test_direct_port_does_not_satisfy_pending_packed_params() {
        let mut f = GxFifo::new();

        f.write_packed(0x0000_0023); // Packed VTX_16, still needs two params.
        f.write_direct(GxCmd::Vtx16, 0x1111_1111); // Separate direct VTX_16 start.

        assert!(
            f.ready.is_empty(),
            "direct VTX_16 param must not complete the packed VTX_16"
        );

        f.write_packed(0xAAAA_AAAA);
        assert!(f.ready.is_empty());
        f.write_packed(0xBBBB_BBBB);

        let packed = f.pop_op().expect("packed op");
        assert_eq!(packed.cmd, GxCmd::Vtx16 as u8);
        assert_eq!(packed.params, vec![0xAAAA_AAAA, 0xBBBB_BBBB]);

        f.write_direct(GxCmd::Vtx16, 0x2222_2222);
        let direct = f.pop_op().expect("direct op");
        assert_eq!(direct.cmd, GxCmd::Vtx16 as u8);
        assert_eq!(direct.params, vec![0x1111_1111, 0x2222_2222]);
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn test_packed_command_params_count_as_entries_as_they_arrive() {
        let mut f = GxFifo::new();
        f.write_packed(0x0000_0023); // VTX_16, two parameters.
        assert_eq!(f.len(), 0);

        f.write_packed(0xAAAA_AAAA);
        assert_eq!(f.len(), 1);
        assert!(f.ready.is_empty());

        f.write_packed(0xBBBB_BBBB);
        assert_eq!(f.len(), 2);
        assert_eq!(f.ready.len(), 1);

        let op = f.pop_op().expect("op");
        assert_eq!(op.cmd, 0x23);
        assert_eq!(op.params, vec![0xAAAA_AAAA, 0xBBBB_BBBB]);
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn test_direct_port_write_past_full_preserves_command_stream() {
        let mut f = GxFifo::new();
        for _ in 0..FIFO_CAPACITY {
            f.write_direct(GxCmd::MtxPush, 0);
        }
        assert!(f.is_full());
        assert!(!f.overflow);
        f.write_direct(GxCmd::MtxPush, 0);
        assert!(!f.overflow);
        assert_eq!(f.len(), FIFO_CAPACITY + 1);
        assert_eq!(f.ready.len(), FIFO_CAPACITY + 1);
        assert_eq!(f.gxstat_high_bits(false) & 0x01FF, 256);
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
        assert!(
            f.take_below_half_edge(),
            "should signal below-half on drain"
        );
    }

    #[test]
    fn test_stat_low_bits_for_empty_fifo() {
        let f = GxFifo::new();
        let s = f.gxstat_high_bits(false);
        assert!(s & (1 << 10) != 0, "empty");
        assert!(s & (1 << 9) != 0, "less than half");
        assert!(s & (1 << 11) == 0, "not busy");
    }

    #[test]
    fn test_less_than_half_irq_uses_decoded_entry_count() {
        let mut f = GxFifo::new();
        f.set_irq_mode(1);

        for _ in 0..32 {
            f.write_packed(0x1515_1515);
            f.write_packed(0);
        }

        assert_eq!(f.len(), FIFO_HALF);
        assert!(
            !f.irq_condition(),
            "128 decoded entries is not less than half full"
        );
    }
}

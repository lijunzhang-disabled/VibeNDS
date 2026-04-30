//! Cycle-based event scheduler. Min-heap; timestamps are in ARM7 cycles
//! (the LCM of the two CPU clocks: 1 ARM7 tick = 2 ARM9 ticks).
//!
//! Ported from `../gba/gba-core/src/scheduler.rs` with the NDS-specific
//! event set.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// Which CPU an event is associated with (when applicable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CpuId {
    Arm9,
    Arm7,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    /// Start of HBlank — visible scanline finished, fire HBlank IRQ if enabled
    /// and run HBlank DMAs (ARM9 only) for visible lines.
    HBlank,
    /// End of HBlank — advance VCOUNT.
    HBlankEnd,
    /// VBlank — line 192 transition. Fires on both CPUs.
    VBlank,

    /// Timer overflow on `(cpu, id)`.
    TimerOverflow(CpuId, u8),

    /// DMA channel completion on `(cpu, channel)`.
    DmaComplete(CpuId, u8),

    /// 32768 Hz audio sample tick.
    AudioSample,

    /// GXFIFO has dropped below half-full — kicks ARM9 DMA mode 7.
    GxFifoLow,

    /// Slot-1 cart command/data transfer complete.
    Slot1Done,

    /// AUXSPI byte transfer complete.
    AuxSpiDone,

    /// 3D engine swap-buffers latched at frame boundary.
    SwapBuffers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub fire_time: u64,
    pub kind: EventKind,
}

impl Ord for Event {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse so BinaryHeap (max-heap) behaves like a min-heap.
        other.fire_time.cmp(&self.fire_time)
    }
}

impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scheduler {
    timestamp: u64,
    events: BinaryHeap<Event>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler { timestamp: 0, events: BinaryHeap::new() }
    }

    pub fn timestamp(&self) -> u64 { self.timestamp }

    pub fn add_cycles(&mut self, cycles: u64) {
        self.timestamp = self.timestamp.wrapping_add(cycles);
    }

    pub fn advance_to(&mut self, time: u64) {
        if time > self.timestamp {
            self.timestamp = time;
        }
    }

    pub fn schedule(&mut self, event: Event) {
        self.events.push(event);
    }

    pub fn peek_time(&self) -> Option<u64> {
        self.events.peek().map(|e| e.fire_time)
    }

    pub fn pop_if_ready(&mut self) -> Option<Event> {
        if let Some(event) = self.events.peek() {
            if event.fire_time <= self.timestamp {
                return self.events.pop();
            }
        }
        None
    }

    pub fn cancel(&mut self, kind: EventKind) {
        let remaining: Vec<Event> = self.events.drain().filter(|e| e.kind != kind).collect();
        self.events.extend(remaining);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_min_heap_ordering() {
        let mut s = Scheduler::new();
        s.schedule(Event { fire_time: 100, kind: EventKind::VBlank });
        s.schedule(Event { fire_time: 50,  kind: EventKind::HBlank });
        s.schedule(Event { fire_time: 75,  kind: EventKind::AudioSample });
        assert_eq!(s.peek_time(), Some(50));
    }

    #[test]
    fn test_pop_if_ready_respects_timestamp() {
        let mut s = Scheduler::new();
        s.schedule(Event { fire_time: 100, kind: EventKind::VBlank });
        assert!(s.pop_if_ready().is_none());
        s.advance_to(100);
        assert_eq!(s.pop_if_ready().unwrap().kind, EventKind::VBlank);
    }

    #[test]
    fn test_cancel_removes_specific_event() {
        let mut s = Scheduler::new();
        s.schedule(Event { fire_time: 1, kind: EventKind::HBlank });
        s.schedule(Event { fire_time: 2, kind: EventKind::VBlank });
        s.schedule(Event { fire_time: 3, kind: EventKind::HBlank });
        s.cancel(EventKind::HBlank);
        s.advance_to(10);
        assert_eq!(s.pop_if_ready().unwrap().kind, EventKind::VBlank);
        assert!(s.pop_if_ready().is_none());
    }
}

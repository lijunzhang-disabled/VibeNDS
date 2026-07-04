//! Timers — 4 per CPU, 8 total. Functionally identical between ARM9 and
//! ARM7; the only difference is which IRQ controller raises overflow IRQs.
//!
//! Each timer is 16-bit counter + reload + control register. Modes:
//! prescaler-driven (F/1, F/64, F/256, F/1024) or cascade (count overflows
//! of timer N-1; timer 0 has no cascade option).
//!
//! Ported from `../gba/gba-core/src/timer.rs`. The NDS uses two instances —
//! one for each CPU's clock domain — but the per-instance ticking logic
//! is unchanged. The caller passes cycles in the appropriate domain.

use serde::{Deserialize, Serialize};

const PRESCALER_DIVIDERS: [u32; 4] = [1, 64, 256, 1024];
const PRESCALER_SHIFTS: [u32; 4] = [0, 6, 8, 10];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timer {
    /// Reload register: written value → reload, read returns counter.
    pub reload: u16,
    pub counter: u16,
    pub control: u16,
    pub(crate) prescaler_counter: u32,
}

impl Timer {
    pub fn enabled(&self) -> bool {
        self.control & (1 << 7) != 0
    }
    pub fn cascade(&self) -> bool {
        self.control & (1 << 2) != 0
    }
    pub fn irq_enabled(&self) -> bool {
        self.control & (1 << 6) != 0
    }
    pub fn prescaler(&self) -> u32 {
        PRESCALER_DIVIDERS[(self.control & 3) as usize]
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timers {
    pub timers: [Timer; 4],
}

#[derive(Debug, Default)]
pub struct TimerTickResult {
    /// Per-timer IRQ requests this tick.
    pub irqs: [bool; 4],
}

impl Timers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read_counter(&self, id: usize) -> u16 {
        self.timers[id].counter
    }
    pub fn read_control(&self, id: usize) -> u16 {
        self.timers[id].control
    }

    pub fn write_reload(&mut self, id: usize, value: u16) {
        self.timers[id].reload = value;
    }

    pub fn write_control(&mut self, id: usize, value: u16) {
        let was_enabled = self.timers[id].enabled();
        self.timers[id].control = value;
        let now_enabled = self.timers[id].enabled();
        if !was_enabled && now_enabled {
            self.timers[id].counter = self.timers[id].reload;
            self.timers[id].prescaler_counter = 0;
        }
    }

    /// Tick all four timers by `cycles` cycles in this CPU's clock domain.
    pub fn tick(&mut self, cycles: u32) -> TimerTickResult {
        let mut result = TimerTickResult::default();

        // This is called once per emulated instruction pair — bail out with
        // one branch when nothing is running.
        if !self.timers.iter().any(Timer::enabled) {
            return result;
        }

        let mut prev_overflow = false;
        for i in 0..4 {
            if !self.timers[i].enabled() {
                prev_overflow = false;
                continue;
            }

            let overflows = if self.timers[i].cascade() && i > 0 {
                if prev_overflow {
                    self.increment(i, 1)
                } else {
                    0
                }
            } else {
                // Prescalers are powers of two (1/64/256/1024) — divide and
                // wrap with shift/mask on this per-instruction path.
                let shift = PRESCALER_SHIFTS[(self.timers[i].control & 3) as usize];
                self.timers[i].prescaler_counter += cycles;
                let ticks = self.timers[i].prescaler_counter >> shift;
                self.timers[i].prescaler_counter &= (1 << shift) - 1;
                if ticks > 0 {
                    self.increment(i, ticks)
                } else {
                    0
                }
            };

            prev_overflow = overflows > 0;
            if prev_overflow && self.timers[i].irq_enabled() {
                result.irqs[i] = true;
            }
        }

        result
    }

    fn increment(&mut self, id: usize, ticks: u32) -> u32 {
        let counter = self.timers[id].counter as u32;
        let reload = self.timers[id].reload as u32;
        const MAX: u32 = 0x10000;

        let total = counter + ticks;
        if total >= MAX {
            let range = MAX - reload;
            if range == 0 {
                self.timers[id].counter = reload as u16;
                return ticks;
            }
            let remaining = total - MAX;
            let extra = remaining / range;
            let final_counter = reload + (remaining % range);
            self.timers[id].counter = final_counter as u16;
            1 + extra
        } else {
            self.timers[id].counter = total as u16;
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tick() {
        let mut t = Timers::new();
        t.write_reload(0, 0xFFF0);
        t.write_control(0, 1 << 7);
        assert_eq!(t.timers[0].counter, 0xFFF0);
        let r = t.tick(10);
        assert_eq!(t.timers[0].counter, 0xFFFA);
        assert!(!r.irqs[0]);
    }

    #[test]
    fn test_overflow_raises_irq() {
        let mut t = Timers::new();
        t.write_reload(0, 0xFFF0);
        t.write_control(0, (1 << 7) | (1 << 6));
        let r = t.tick(20);
        assert_eq!(t.timers[0].counter, 0xFFF4);
        assert!(r.irqs[0]);
    }

    #[test]
    fn test_prescaler_64() {
        let mut t = Timers::new();
        t.write_reload(0, 0);
        t.write_control(0, (1 << 7) | 1);
        t.tick(63);
        assert_eq!(t.timers[0].counter, 0);
        t.tick(1);
        assert_eq!(t.timers[0].counter, 1);
    }

    #[test]
    fn test_cascade() {
        let mut t = Timers::new();
        t.write_reload(0, 0xFFFF);
        t.write_control(0, 1 << 7);
        t.write_reload(1, 0);
        t.write_control(1, (1 << 7) | (1 << 2));
        let r = t.tick(1);
        assert!(!r.irqs[0]); // no IRQ enable
        assert_eq!(t.timers[1].counter, 1);
    }

    #[test]
    fn test_reload_on_enable() {
        let mut t = Timers::new();
        t.write_reload(0, 0x1234);
        assert_eq!(t.timers[0].counter, 0);
        t.write_control(0, 1 << 7);
        assert_eq!(t.timers[0].counter, 0x1234);
    }
}

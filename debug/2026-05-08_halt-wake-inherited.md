# Halt-wake bug — inherited from GBA port

Date: 2026-05-08
Status: **Fixed**
Phase context: between Phase 4 and Phase 5. Surfaced during a routine sweep of `../gba/debug/` for entries added since the 2026-04-27 inheritance audit.

## Summary

Our `Nds::run_cycles` skips `step()` entirely while a CPU is halted (it fast-forwards). But `step()` is the only place that clears `halted`. So once either CPU executes `SWI 0x02 Halt`, `SWI 0x04 IntrWait`, or `SWI 0x05 VBlankIntrWait`, it sleeps forever — even though the scheduler keeps firing VBlank/HBlank/timer events that set IF bits.

Real ARM7TDMI / ARM946E-S behavior: **halt-wake is gated by `(IE & IF) != 0` alone.** `IME` and `CPSR.I` gate IRQ *delivery* but not halt *exit*. The CPU wakes whenever any enabled IRQ source is pending, regardless of whether it can immediately take the IRQ.

Same root cause as the GBA project's commit `27722c4` (2026-04-30), which we inherited verbatim along with the rest of `lib.rs::run_cycles`. We picked it up during a follow-up audit pass on 2026-05-08 — three new entries had landed in `../gba/debug/` since our 2026-04-27 sweep.

## Why our tests didn't catch it

- `test_vblank_irq_fires_on_both_cpus` halts both CPUs and runs a frame, but only asserts IF gets set — never expects the CPUs to *wake*.
- `test_vcount_advances_through_full_frame` halts both CPUs but checks only `vcount`.
- Our BIOS HLE `swi_intr_wait` sets `cpu.halted = true` but no test exercises a wake afterward.

In short, every existing halt-related test stopped one step shy of the wake assertion. The bug only surfaces in real ROMs that issue a halt-wait SWI at boot and expect to be woken by VBlank.

## Fix

`nds-core/src/interrupt.rs` — new method:

```rust
/// True when any enabled IRQ is pending, **ignoring** IME and CPSR.I.
pub fn has_unmasked_irq(&self) -> bool {
    (self.ie & self.iflag) != 0
}
```

`nds-core/src/lib.rs::run_cycles` — after the `dispatch_event` drain loop, clear `halted` on either CPU whose controller has an unmasked IRQ:

```rust
while let Some(event) = self.scheduler.pop_if_ready() {
    self.dispatch_event(event);
}

if self.cpu9.halted && self.shared.irq9.has_unmasked_irq() {
    self.cpu9.halted = false;
}
if self.cpu7.halted && self.shared.irq7.has_unmasked_irq() {
    self.cpu7.halted = false;
}
```

The next outer-loop iteration's `step()` then delivers the IRQ (if IME is also set) through the existing path.

## Regression tests

In `cpu/tests` / `lib.rs::tests`:

1. **`test_halt_wake_on_unmasked_vblank_irq`** — configures DISPSTAT VBlank IRQ enable + `IE.VBlank` on both controllers, leaves `IME = 0` (so IRQ delivery is gated but halt-wake should still fire), halts both CPUs, runs a frame, asserts:
   - `IF.VBlank` set on both controllers.
   - Both CPUs `halted = false`.

2. **`test_halt_stays_halted_when_no_irq_enabled`** — negative case: halt with no `IE` bits set, run a frame, assert both CPUs remain halted. Catches a "wake on any scheduler tick" regression.

I verified the first test catches the bug by temporarily commenting out the wake check — it fails exactly at the `cpu9.halted == false` assertion. Restored fix; 140/140 pass.

## Other entries audited in this pass

Three new GBA `debug/` entries landed between 2026-04-27 and 2026-05-08:

| Date | Subject | Status for us |
|---|---|---|
| 2026-04-30 | SRTOG Flash chip-ID detection | **N/A** — GBA-specific Flash backup. NDS uses AUXSPI (serial protocol). |
| (cmt `27722c4`) | **Halt-wake** | **THIS DOC** |
| 2026-05-04 | `std::env::var` hot-path perf | **N/A now**, **lesson for later**. Grep confirmed zero `std::env::var` calls in `nds-core/`. When we add diagnostic switches (in Phase 5+ as we need them), use the `OnceLock<bool>` cache pattern, not raw `env::var` in hot paths. |
| 2026-05-05 | SRTOG FIFO_B cross-trigger | **N/A**, **lesson for Phase 8**. GBA-specific audio FIFO arrangement. General lesson: when one event can cross-trigger multiple armed DMA channels, gate by *actual demand* per channel, not by class. Relevant for NDS sound DMA where 4 channels can be armed for `DmaTiming::Special`. |

## Lessons

- **The audit pattern works.** Two passes (one on 2026-04-27, one on 2026-05-08), two inherited bugs caught (IRQ pipeline-refill + halt-wake), both fixed for the cost of a regression test each. Worth re-running before any future port milestone.
- **Halt is sneaky in skip-step run loops.** Any future top-level loop that fast-forwards instead of calling `step()` needs an explicit wake check at the boundary. Same caution applies if we ever switch from "lockstep at 1 ARM7 cycle" to coarser interleaving — the wake check must fire at each chunk boundary, not just frame boundary.
- **"Halts a CPU and runs a frame" tests need a wake assertion.** Easy to add one assertion to the end of any existing halt test.

## Related

- `../gba/debug/2026-04-30_srtog-flash-chip-id.md` — the GBA investigation that found the halt-wake bug as a side-finding.
- `../gba/` commit `27722c4` — the canonical GBA fix.
- `debug/2026-04-27_irq-pipeline-refill-inherited.md` — earlier inherited bug fixed by the same audit process.
- Memory record: [`Audit upstream debug/ when porting from ../gba`](../../.claude/projects/-Users-lijunzhang-secagentinfra-nds/memory/feedback_check_upstream_debug.md) — practice this audit validated.

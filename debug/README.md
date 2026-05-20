# Debug folder

Per-bug investigation notes, named `YYYY-MM-DD_<short-slug>.md`.

Same convention as the sibling GBA project at `../gba/debug/`. Each entry
should be self-contained: symptom → investigation → root cause → fix →
regression test → verification.

## Bug index

| Date | Bug | Status |
|---|---|---|
| 2026-04-27 | [IRQ pipeline-refill ordering inherited from GBA port](2026-04-27_irq-pipeline-refill-inherited.md) | **Fixed** |
| 2026-05-08 | [Halt-wake — halted CPU never woken by pending IRQ (inherited)](2026-05-08_halt-wake-inherited.md) | **Fixed** |

## Phase 9 working docs

- **[`phase9_carryover.md`](phase9_carryover.md)** — consolidated list of every item deferred during Phases 1-7. ~22 items across 3D engine, CPU/bus accuracy, cart/boot, audio, and diagnostics. Pull entries into dated investigation logs as you work them.
- **[`test-plan.md`](test-plan.md)** — ordered test-ROM plan for Phase 9: armwrestler-ds → rockwrestler → devkitPro 3D examples → commercial titles (Phoenix Wright first). Each stage assumes the previous is passing.

## Lessons for later phases (from upstream audit)

- **Phase 5+** (when adding diagnostic switches): cache env-var lookups via `OnceLock<bool>`, not raw `std::env::var` in hot paths. See `../gba/debug/2026-05-04_env-var-hot-path-perf.md` — caused a 10× slowdown on the GBA before they caught it.
- **Phase 8** (audio): when one event (timer overflow, FIFO drain, etc.) can cross-trigger multiple armed DMA channels, gate by *actual demand* per channel, not by class. See `../gba/debug/2026-05-05_srtog-fifo-b-cross-trigger.md`.

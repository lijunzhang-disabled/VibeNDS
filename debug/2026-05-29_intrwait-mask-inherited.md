# IntrWait mask — inherited from GBA port

Date: 2026-05-29
Status: **Fixed** (this commit)

## Symptom (predicted — pre-Phase 9)

When a commercial NDS title runs its main loop with `WaitForVBlank()` /
`SWI 0x05 VBlankIntrWait`, the CPU should park until VBlank fires (~once
per 60 Hz frame). Real games rely on this: their main loop is

```
loop {
    update_state();
    SoundMain();        ← mixes ONE audio frame
    SWI 0x05;           ← park until next VBlank
}
```

Our HLE for SWI 0x04 / 0x05 (ARM9 and ARM7) collapsed to a single
`cpu.halted = true`, after which the run-loop's halt-wake check wakes the
CPU on *any* pending IRQ (HBlank, Timer, IPC, DMA…). On a frame with
~263 line events (192 visible HBlanks + VBlank + …) plus timer overflows
and IPC traffic, the main loop would iterate dozens of times per frame
instead of once. Predicted symptoms:

- Audio engines (M4A on GBA, MP2K-derived engines on DS) overflow their
  intermediate channel buffers → corruption → IRQ-handler self-overwrite
  → cascade.
- 2D effects that update per-VBlank (BG scroll, palette fades) tick at
  ~10× the intended rate → animation runs absurdly fast or visibly
  glitches.
- IPC-driven games (everything with touch input) flood the ARM7 queue.

## Upstream history (`../gba`)

The same bug existed in our GBA emulator. It surfaced as Fire Emblem 7's
intro-hang (commercial title boot break), traced over a ~5-day
investigation in `gba/debug/2026-05-24_fe7-hblank-irq-cascade.md`. The
diagnosis that survived was:

> Real BIOS implements SWI 0x04/0x05 as
> `loop { HALT; if BIOS_IF & wait_mask != 0 { break; } }` — i.e., a
> non-matching IRQ wakes from HALT but the loop immediately re-halts.
> Net effect: CPU stays halted until a *matching* IRQ fires.

The GBA fix (commit `bb4b916`, ~10 lines) added a `cpu.intrwait_mask`
field, set by the SWI HLE, and gated the halt-wake check on it. Earlier
attempts (commits `18b39cb`/`674c34c`) also modelled BIOS_IF mirror
writes inside `handle_interrupt` — those conflicted with games' user
IRQ handlers and were reverted. The clean version doesn't touch BIOS_IF
at all; it just produces the same observable semantics as the BIOS loop
without emulating its implementation details.

## Verification audit

NDS upstream-audit sweep (per [[feedback_check_upstream_debug]]) on
2026-05-29 caught it before Phase 9 test ROMs surfaced it:

- `nds-core/src/bios/arm9.rs:29` — `swi_intr_wait(cpu) { cpu.halted = true; }`
- `nds-core/src/bios/arm7.rs:25` — same shape
- `nds-core/src/lib.rs` halt-wake post-dispatch block — wakes on any
  `has_unmasked_irq()`, no mask gate

Same shape as GBA pre-`bb4b916`. The NDS would have hit this on
practically every commercial title (Phoenix Wright is the first one we
planned to try in Phase 9 — see `debug/test-plan.md`).

## Fix (this commit, ~25 lines)

1. **`nds-core/src/cpu/mod.rs`** — add `pub intrwait_mask: u32` to
   `Cpu`. Default 0 (= HALTCNT-style: any IRQ wakes). Non-zero =
   IntrWait mask. Cleared on wake.

2. **`nds-core/src/interrupt.rs`** — add `has_matching_irq(mask)`
   helper. Sibling of the existing `has_unmasked_irq()`.

3. **`nds-core/src/lib.rs`** halt-wake block — when `intrwait_mask !=
   0`, gate the wake on `has_matching_irq(mask)`; otherwise existing
   `has_unmasked_irq()`. On wake, clear both `halted` and the mask.

4. **`nds-core/src/cpu/mod.rs::step()`** — when an IRQ is actually
   delivered (the normal `bus.irq_pending()` path), also clear
   `intrwait_mask`. Defensive — covers the case where halt-wake fires
   on a non-IntrWait path but a stale mask survives.

5. **`nds-core/src/bios/arm9.rs` + `arm7.rs`** — `swi_intr_wait` reads
   `R1` (IRQ flags mask), writes it into `cpu.intrwait_mask`, then
   halts. Zero mask defaults to `0xFFFF_FFFF` (= wake on any IRQ) so
   buggy games that pass an empty mask don't park forever — matches
   the GBA fix.

We deliberately do NOT model the BIOS_IF mirror at IWRAM `0x03FFFFF8` /
`0x03007FF8`. The GBA experiment showed it adds breakage without buying
correctness; the IntrWait mask gate is sufficient to produce the right
observable behavior.

## Regression tests

`nds-core/src/lib.rs::tests`:

- `test_intrwait_mask_blocks_non_matching_irq` — park CPU with VBlank
  mask, raise HBlank, confirm CPU stays halted and mask survives.
- `test_intrwait_mask_wakes_on_matching_irq` — park with VBlank mask,
  raise VBlank, confirm CPU wakes and mask clears.

Plus the pre-existing `test_halt_wake_on_unmasked_vblank_irq` (HALTCNT-
style, mask=0) continues to pass — proves the zero-mask path still
works for SWI 0x02 (Halt).

Full suite: 263 / 263 passing (was 261; +2 new).

## Files changed

- `nds-core/src/cpu/mod.rs` — add `intrwait_mask` field; clear on IRQ delivery
- `nds-core/src/interrupt.rs` — add `has_matching_irq()`
- `nds-core/src/lib.rs` — gate halt-wake on mask
- `nds-core/src/bios/arm9.rs` — set mask in SWI 0x04/0x05
- `nds-core/src/bios/arm7.rs` — set mask in SWI 0x04/0x05
- `nds-core/src/lib.rs::tests` — two new regression tests

## Lessons

1. The [[feedback_check_upstream_debug]] audit pattern paid off again.
   Without it, this would have surfaced as "Phoenix Wright boots to a
   buzzing white screen and the dialogue advances by itself" — fixable,
   but a fresh ~5-day FE7-style investigation from scratch. With it,
   it's ~30 minutes of reading + ~25 lines of code.

2. As with the GBA fix, do NOT also model BIOS_IF. The intrwait_mask
   gate alone produces the right observable semantics. Modelling
   BIOS_IF in `handle_interrupt` fights with user IRQ handlers that
   maintain their own state.

3. A zero mask (R1 = 0) is a buggy-game footgun, not a "wait for
   nothing" semantic. Map it to "wake on any IRQ" so we degrade
   gracefully rather than parking forever.

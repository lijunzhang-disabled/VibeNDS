# IRQ pipeline-refill ordering — bug inherited from GBA port

Date: 2026-04-27
Status: **Fixed**
Phase context: between Phase 4 and Phase 5. No game has actually triggered the bug for us yet — found by auditing the GBA project's `debug/` folder.

## Summary

We ported `cpu/mod.rs` from `../gba/gba-core/src/arm7tdmi/mod.rs` on **2026-04-26**. The GBA project shipped a CPU correctness fix for IRQ-pipeline-refill ordering on **2026-04-29** — three days *after* our port. We inherited the buggy shape verbatim.

This report records the inheritance, points to the GBA's primary investigation as the canonical reference, and confirms the fix lands cleanly in our port.

## Why we have the bug

Comparing our `Cpu::step` (pre-fix) with the GBA file we copied from:

```rust
pub fn step<B: CpuBus>(&mut self, bus: &mut B) -> u32 {
    if bus.irq_pending() && !self.cpsr.irq_disabled() {
        self.irq_entries += 1;
        self.handle_interrupt();   // ← reads regs[15] BEFORE the refill repairs it
        self.halted = false;
    }

    if self.halted { return 1; }

    if self.pipeline_flushed {
        self.refill_pipeline(bus); // ← too late
    }

    if self.cpsr.thumb() { self.step_thumb(bus) } else { self.step_arm(bus) }
}
```

Identical shape to GBA's pre-fix `step`. The bug applies to both ARM7 and ARM9 since we use a single `Cpu` struct for both.

## What's wrong (executive summary)

Any PC-writing instruction (`B`, `BX`, `LDR PC`, `MOV PC`, `LDM` with R15, exception entry) leaves `regs[15] = raw_target` and `pipeline_flushed = true`. The ARM7TDMI invariant — "during execution, `regs[15] = exec + 8` (ARM) or `+ 4` (THUMB)" — is **temporarily broken** between that branch and the next refill.

If `bus.irq_pending()` returns true on the *very next step*, `handle_interrupt` reads `regs[15] = raw_target` and saves:

| Mode  | `regs[15]` actually | `LR_irq` becomes | Should have been | Off by |
|-------|---------------------|------------------|------------------|--------|
| THUMB | `target`            | `target`         | `target + 4`     | -4     |
| ARM   | `target`            | `target − 4`     | `target + 4`     | -8     |

The standard BIOS exception-return `SUBS PC, LR, #4` then resumes at `target − 4` (THUMB) or `target − 8` (ARM) — inside the previous instruction or a literal-pool word.

The full causal chain plus the GBA project's measured failure rate (~60% per Pokémon save flow) is documented in:

→ **[`../../gba/debug/2026-04-29_pokemon-save-irq-pipeline-refill.md`](../../gba/debug/2026-04-29_pokemon-save-irq-pipeline-refill.md)** ←

That doc is the canonical reference. This file just records the inheritance and our patch.

## Fix

Move the refill to run **before** the IRQ check. A second refill is needed *after* the IRQ check because IRQ entry itself flushes (sets `regs[15]` to the vector). Both refills are guarded by `pipeline_flushed` and idempotent.

`nds-core/src/cpu/mod.rs:`

```rust
pub fn step<B: CpuBus>(&mut self, bus: &mut B) -> u32 {
    // 1. Refill before the IRQ check — establishes regs[15] = exec + 4/8
    //    so handle_interrupt's LR_irq math is correct.
    if self.pipeline_flushed {
        self.refill_pipeline(bus);
    }

    if bus.irq_pending() && !self.cpsr.irq_disabled() {
        self.irq_entries += 1;
        self.handle_interrupt();
        self.halted = false;
    }

    if self.halted { return 1; }

    // 2. Refill again — IRQ entry flushes (sets regs[15] to the vector).
    if self.pipeline_flushed {
        self.refill_pipeline(bus);
    }

    if self.cpsr.thumb() { self.step_thumb(bus) } else { self.step_arm(bus) }
}
```

The fix is identical in shape to the GBA's commit `b29226f`.

## Regression test

`cpu::tests::test_irq_during_pipeline_flushed_window_resumes_at_branch_target` in `nds-core/src/cpu/mod.rs`. Constructs the exact buggy window:

- `0x100`: `B +0x10`  (target 0x118)
- `0x118`: `MOV R5, #0x42`
- `0x18`: `SUBS PC, LR, #4`  (planted IRQ handler)

Sequence:
1. Step #1 — execute the branch. Asserts `regs[15] = 0x118` and `pipeline_flushed = true`.
2. Set `bus.irq = true`. Step #2 — IRQ fires inside the flushed window, handler runs to completion.
3. Asserts:
   - `banked.lr[Irq] == 0x11C` (= branch_target + 4). Pre-fix this is `0x114`.
   - mode is back to System, `regs[15] == 0x118` (resumed correctly).
4. Step #3 — execute `MOV R5, #0x42`. Asserts `R5 == 0x42`.

I verified the test catches the bug by temporarily reverting the fix: `banked.lr[Irq]` becomes `276 (= 0x114)`, exactly the pre-fix value.

## Verification

- 138/138 unit tests pass.
- `cargo build` and `cargo build --release` clean.
- The 137 pre-existing tests still pass — the additional refill is invisible to non-IRQ paths because `pipeline_flushed` is `false` on the second invocation.

## Other GBA debug entries audited at the same time

When this bug surfaced from the GBA debug folder, I swept the other entries to see what else we'd inherited. Status:

| GBA bug | Status in our port |
|---|---|
| 2026-04-22 pipeline advance ordering + MRS/MSR mask 0xF9→0xFB | ✓ Fixed at port time |
| 2026-04-24 MSR mode-bit banking silently skipped | ✓ Fixed at port time |
| 2026-04-24 CPU sweep (5 fixes: LDRH rotation, PC+12 shift-by-reg, P-variants, LDR Rn==Rd writeback) | ✓ All fixed at port time |
| 2026-04-25 Pokémon FIFO DMA reanchor | N/A — GBA-specific (M4A driver) |
| 2026-04-26 Pokémon save Flash 8-bit bus | N/A — NDS uses AUXSPI, not memory-mapped Flash |
| **2026-04-29 IRQ pipeline-refill** | **THIS DOC** |

## Followups carried over from `../gba/debug/followups.md`

These apply to us too and stay open:

- Wait state emulation (`WAITCNT` analogue on NDS — `EXMEMCNT` for slot-2 timing): all our memory accesses are 1 cycle; real hardware varies. Phase 9.
- Open-bus read accuracy: same simplification.
- Misaligned ARM access quirks: the ARM7-side LDRH/LDR rotations are in; want a focused pass against the ARM ARM spec eventually.

## Lessons

**Always check the upstream debug history when porting.** The GBA's `debug/` folder represents months of CPU-correctness debugging that we'd otherwise rediscover the hard way one game at a time. We caught one bug here for the cost of one regression test; the GBA project found the same bug after a multi-day investigation into Pokémon save corruption. Worth repeating before any future ports or rebases.

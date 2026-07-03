# HeartGold player texture missing — root cause: instant Slot-1 reads overflow the game's VBlank task queue

Date: 2026-07-03
Status: **Fixed** (Slot-1 card transfer timing implemented; verification below)

## Symptom

In the player's bedroom (intro), the player billboard (polygon 338,
tex=0x2D2314BE → texture VRAM 0xA5F0, palette base 0x9F0) renders as black
striped fragments. A debug re-render that manually uploads the correct texel
data (main RAM 0x02338254) and palette (0x023383F4) to those VRAM offsets
renders correctly, so rasterization/decoding/depth were already ruled out.

## Investigation chain

1. **The upload never happens.** Full-run traces (`NDS_TRACE_VRAM_BANK_RANGE`,
   `NDS_TRACE_DMA9_REG_DEST_RANGE`, frame markers) showed the only write ever
   landing at texture offset 0xA5F0 is the *intro's* whole-arena upload at
   frame ~427 (63.5 KB DMA from 0x022B4D64 → LCDC 0x06800000; 0x022B4D64 +
   0xA5F0 = 0x022BF354 = the "stripe" source). The f_hero texel data never
   reaches any VRAM bank; neither does its palette. The stripes are stale
   intro data in a freshly allocated slot.

2. **Room load flow.** At the room load (frames ~17796-17803), textures
   upload via two paths (both direct `GX_LoadTex`-style DMAs during VBlank,
   not the NNS_Gfd VRAM-transfer queue):
   - combined alloc+load (`gf_3d_loader`, lr≈0x0201F6AD/0x0201F76D)
   - two-phase raw-res manager (`unk_02025534.c`):
     `AllocVramAndGetKeys` one frame, `LoadTex` the next, each step deferred
     as a **SysTask on gSystem.vblankTaskQueue**.

3. **The player's sheet allocates but never loads.** Live traces from a
   state at frame 17650 show `NNS_GfdAllocLnkTexVram(0x3000)` → key 0x7BF0
   (the 24-frame player sheet; frame 21 = 0xA5F0) and a successful palette
   key 0x9F0 — but no `ResTexLoad` ever fires for it.

4. **Root cause: SysTask VBlank queue overflow.** pokeheartgold decomp:
   `SysTaskQueue_InsertTask` **silently returns NULL when the queue is full**;
   `gSystem.vblankTaskQueue` holds **32 tasks** (`system.c`). Tracing
   `SysTaskQueue_InsertTask` (0x0201F8C0) + its fail-return (0x0201F8E6)
   showed **44 inserts into the vblank queue at frame 17800 → 12 silently
   dropped**, including the billboard-sprite manager's batched texture-load
   task (`func=0x021FA6E1, data=0x022A1ED8` — the manager that owns the
   player's sheet).

5. **Why hardware doesn't overflow:** `VBlankCB_DmaTasksFramecounter` is the
   VBlank *ISR* — it drains the queue every VBlank even while the main
   thread is blocked in synchronous FS reads. Real card reads pace the load:
   our run moved **394 card blocks (201 KB) in a single frame** (frame
   17798); HGSS reads with ROMCTRL=0xA1416657 (gap1=0x657, 6.7 MHz clock →
   5 cycles/byte) take ≈4,223 ARM7 cycles per 0x200 block on hardware, so
   that frame's reads alone are ~3 frames of real time. With instant reads,
   an entire map-load's worth of object creation lands in one frame and
   blows the 32-entry queue.

## Fix

Implemented Slot-1 card transfer timing in nds-core:

- `start_slot1_transfer` now queues generated words into `slot1_pending`
  and requests a delay of `gap1 + gap2*(pages-1) + rate*(8 + bytes)` ARM7
  cycles (rate 5 or 8 per ROMCTRL bit 27). Busy (bit 31) is set immediately;
  data-ready (bit 23) stays clear.
- The frame loop turns the request into a scheduled `EventKind::Slot1Done`.
  On fire: pending words move to `slot1_data`, Slot1-timed DMAs run on both
  CPUs (`run_slot1_dmas`, extracted from the old immediate-fire paths), and
  the transfer-complete IRQ is raised (previously raised at command start).
- ROMCTRL writes no longer fire Slot-1 DMA immediately.
- Gotcha: `run_frame` uses `run_cycles`, which has its own step loop and does
  NOT call `step_one` — the delay-request→event conversion
  (`pump_slot1_schedule`) must run in BOTH loops. The first attempt only
  wired `step_one`, which hung the game on its first card read (one
  `slot1 start`, no completion, ARM9 busy-polling forever).

Slot-1 unit tests updated to the delayed contract (they now call
`complete_slot1_transfer()` to simulate the event; new asserts cover
"no data / no IRQ before completion"). 661/661 nds-core tests pass.

## Game-side references (US HGSS, via pret/pokeheartgold)

- `NNS_GfdRegisterNewVramTransferTask` 0x020B634C, manager global 0x021D84D0
- `GF_CreateNewVramTransferTask` 0x020205D8, `sVramTransferManager` 0x021D2194
- `NNS_G3dTexLoad` 0x020BE418, `NNS_G3dPlttLoad` 0x020BE538
- `NNS_GfdAllocLnkTexVram` 0x020B6814
- raw-res manager (`unk_02025534.c`): `ResTexAllocVramAndGetKeys` 0x02025B40,
  `ResTexLoad` 0x02025BB0, `LoadTex` 0x020259B0, `LoadObjTexById` 0x020259E0
- `SysTaskQueue_InsertTask` 0x0201F8C0 (fail return 0x0201F8E6),
  `SysTask_CreateOnVBlankQueue` 0x0200E33C, `gSystem` 0x021D110C,
  vblankTaskQueue instance 0x023BDC78 (limit 32)
- VBlank ISR `VBlankCB_DmaTasksFramecounter` drains the queue every frame

## Verification (done 2026-07-03)

- 661/661 nds-core tests pass (9 slot1 tests updated to the delayed
  contract, with new "no data / no IRQ before completion" asserts).
- Full intro replay (same action script as the failing capture) reaches the
  bedroom with the player sprite rendered correctly:
  `/private/tmp/hg-slot1timing-verify/capture-3d.png`. The result is cleaner
  than the manual-upload debug rerender
  (`/private/tmp/hg-fhero-upload-rerender/patched-3d.png`), whose remaining
  black fragments were the *other* 11 dropped vblank tasks — also fixed by
  the pacing change. Broken baseline for comparison:
  `/private/tmp/hg-current-normal-capture/capture-3d.png`.
- The player polygon now references a texture slot containing real sprite
  texel data (slot offsets shift vs. the broken run because allocation
  order changes with the new timing — expected).
- Boot speed is normal: ~2,500 card block reads flow through the first 300
  frames; whole replay wall time unchanged (~3 min).
- Fresh bedroom save-state compatible with the new build:
  `/private/tmp/hg-room-fixed.state`.

## Notes / possible follow-ups

- The 10 other dropped vblank tasks at frame 17800 (`func=0x02069701`) were
  per-map-object tasks; with the pacing fix these also survive.
- Our timing model is a lower bound on real card latency (no MROM page seek
  beyond gap1). If some other title still bunches loads too tightly, revisit
  with measured per-block latencies.
- The transfer-complete IRQ now fires at completion after DMA drain, which
  matches DMA-driven SDK drivers; CPU-polling drivers see all words arrive
  at once at completion (block granularity, not per-word).

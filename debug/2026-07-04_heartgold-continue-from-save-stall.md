# HeartGold "CONTINUE" from a saved game is very slow (~30 s), not broken

Date: 2026-07-04 (revised 2026-07-06)
Status: **Open** — characterized; root cause narrowed to a ~30 s timeout on a
missing async completion. Pre-existing (reproduces on 2d2d463, before the
recent perf / save-state work).

## Correction

An earlier revision of this note called this a hard stall. **That was wrong**
— it does load, it just takes ~27–32 s. The first investigation gave up at
~15 s of black screen. Letting it run longer, the overworld (New Bark Town)
renders correctly. It's a severe *slowness*, not a hang.

## Symptom

Title → touch/START → menu shows **CONTINUE** with the correct save preview
(PLAYER/TIME/BADGES read fine). Selecting CONTINUE goes black for ~30 s, then
the overworld appears normally. NEW GAME reaches the overworld promptly, so
boot / input / map-render are fine — the slowness is specific to resuming a
saved game.

Distinct from the save-*state* (F5/F8) speed fix and from in-game save
*writing* (authentically slow).

## What's established (measured from a post-CONTINUE state)

Timeline after selecting CONTINUE (frame numbers approximate):

- **Frames 0–22:** ARM9 active — steady ~8 IPC-FIFO sends/frame (per-frame
  sound/field driver chatter) plus a few slot-1 card reads. Then it stops.
- **Frames ~22–1530 (the ~25 s wait):** *complete silence.* No slot-1 card
  reads, no AUXSPI backup I/O, no RTC reads, no IPC FIFO, no IPCSYNC. The
  ARM9 main thread is asleep (halted, waking only for the VBlank handler);
  the ARM7 runs its normal 16-channel sound loop. Over 600 idle frames only
  four RAM words change — two per-frame frame counters and a free-running
  ~16 Hz timer heartbeat (a timer IRQ handler at `0x02025418`, reload 0,
  ÷64 prescaler — firing correctly, a red herring). Nothing counts toward a
  visible threshold.
- **Frame ~1531 onward (~75 frames):** everything springs to life at once —
  IPC bursts, 539 AUXSPI flash-read transactions (the full save block),
  2400+ slot-1 card reads (the map/graphics), then the overworld renders.

## Leading hypothesis

The ARM9 blocks around frame 22 on an **async operation kicked off in
frames 0–22** and polls a completion flag that a callback/IRQ should set.
Because we never deliver that completion, it waits the full ~1500-frame
(~30 s) **timeout**, then proceeds with a fallback that does the load
synchronously (the frame-1531 burst). This matches: it always takes the same
~30 s, always eventually succeeds, and there is zero I/O during the wait
(the game is polling a memory flag, not doing hardware work).

Candidate missing completion (kicked in frames 0–22):

1. A slot-1 card read / DMA whose transfer-complete or DMA-complete IRQ we
   don't raise for this mode (the write path needed exactly such a fix; the
   read/async path may need the same).
2. A sound-driver command to the ARM7 (load field BGM) whose "done" the ARM7
   should signal back — note sound-DMA is a documented Phase 8/9 carry-over.

## Next steps

- Instrument the wait's poll: find the exact memory flag the ARM9 checks each
  VBlank before re-sleeping (trace ARM9 reads in the idle, gate on the branch
  that eventually falls through at frame 1531), then find who *should* set it.
- Correlate the frames-0–22 slot-1 card reads with their expected completion
  IRQ; verify the transfer-complete / DMA IRQ actually fires and is unmasked.
- Check the ARM7 sound path for a load-complete signal the ARM9 awaits.

## Tooling added while investigating (kept — all gated, zero-cost off)

- `NDS_TRACE_ARM7_EXEC_RANGE=start..end` — ARM7 exec PC trace (mirror of the
  ARM9 one).
- harness `dump_arm7_ram_raw` (region `private`|`shared`, offset, len) — read
  ARM7 WRAM for disassembly.
- `NDS_TRACE_IPC=1` — trace IPC FIFO sends and IPCSYNC writes on both sides.

## Repro

Boot HGSS with a valid `.sav`; from title START → A (menu) → A (CONTINUE),
then step ~1900 frames — black for ~30 s, then New Bark Town. Fast-iteration
states: `/private/tmp/hg-menu.state` (at the CONTINUE menu),
`/private/tmp/hg-stall.state` and `/private/tmp/hg-mididle.state` (inside the
wait). All need `load_rom` first (ROM no longer lives in save states).

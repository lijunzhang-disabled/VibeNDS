# HeartGold "CONTINUE" from a saved game stalls on a black screen

Date: 2026-07-04
Status: **Open** — characterized, root cause not yet found. Pre-existing
(reproduces on 2d2d463, before the recent perf / save-state work — NOT a
regression from those).

## Symptom

From the title: touch/START → main menu shows **CONTINUE** with the correct
save preview (PLAYER AA, TIME 0:04, BADGES 0) — so the save is read fine.
Selecting CONTINUE clears the menu and both screens go **black and stay
black** indefinitely (≥15 s emulated observed). NEW GAME works and reaches
the overworld normally, so boot/input/map-render are all fine — the bug is
specific to resuming a saved game.

This is distinct from the save-*state* (F5/F8) speed issue fixed the same
day, and from in-game save *writing* (which is authentically slow). This one
is a hard stall, not slowness.

## What's established

- **Not a CPU spin.** Exec trace over the stall shows the ARM9 mostly
  halted; the only hot code is a bounded 22-iteration per-frame loop at
  `0x020D1028–0x020D107C` (looks like OAM/matrix setup run from the VBlank
  path), ~44 hits over 8 frames.
- **Main thread is blocked; the VBlank ISR still runs.** Over 120 frames
  only four words in main RAM change, all counters:
  - `0x021D113C` +120 (gSystem+0x30 = `frameCounter`, ticked by
    `VBlankCB_DmaTasksFramecounter` every VBlank — so the ISR is alive)
  - `0x023FFC3C` +120 (another per-frame counter)
  - `0x021D2214` +32, `0x021E19DC` +32 (slower, ~1 per 3.75 frames)
  So the game is alive but the main thread is parked in an OS wait
  (`OS_SleepThread` / message-queue receive / `CARD_WaitBackupAsync`-style
  block) and the wake never comes.
- **Display is on but empty.** `engine_a_dispcnt = 0x00011710` (mode 1,
  BG0/1/2 + OBJ enabled), `disp3dcnt = 0x19` — the game enabled the layers
  but their char/map data was never loaded because the load never finished.
- **No backup SPI during the stall.** `NDS_TRACE_AUXSPI` is silent across
  the stuck window — the save-block reads already happened; the game is now
  waiting on a *later* async step's completion.

## Hypothesis (for next session)

The continue path fires an async operation and blocks a thread on its
completion signal, which we never deliver. Two leading candidates:

1. **Async backup read completion.** HGSS `FlashLoadChunk` uses
   `CARD_ReadBackupAsync` + `CARD_WaitBackupAsync`; `WaitBackupAsync`
   sleeps the calling thread until the card/backup transfer-complete
   callback runs. The write path needed the transfer-complete IRQ (fixed in
   `2026-07-04_heartgold-save-flash-ir-backup.md` era) — the async *read*
   completion may need an equivalent signal we don't raise for this mode.
2. **IPC / ARM7 card thread.** On the DS the card+backup live on the ARM7;
   the ARM9 requests via IPC and waits for the ARM7's reply. If the ARM7
   card/backup completion or its IPC signal-back doesn't fire for this
   larger async read, the ARM9 blocks forever.

## Next steps

- Trace the transition *into* the wait: exec-trace the last main-thread
  code before the freeze to find the exact poll/sleep and the flag/queue it
  waits on.
- Trace ARM7 exec during the stall to see if the ARM7 is spinning in a
  card/backup wait (points at candidate 2) or idle (points at candidate 1).
- Check whether the async backup read raises the AUXSPI transfer-complete
  IRQ (bit 14) and whether the SDK's async path expects it, mirroring the
  write-path fix.

## Repro

Boot HGSS with a valid `.sav`, from title: START → A (to menu) → A (select
CONTINUE), then step ~900 frames — both screens stay black. Saved stall
state for fast iteration: `/private/tmp/hg-stall.state` (needs `load_rom`
first, per the ROM-not-in-state change).

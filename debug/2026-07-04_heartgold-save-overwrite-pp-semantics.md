# HeartGold save-overwrite failure — FLASH page-program must overwrite, not AND

Date: 2026-07-04
Status: **Fixed** (follow-up to `2026-07-04_heartgold-save-flash-ir-backup.md`)

## Symptom

After the FLASH-512K/IR fix, the **first** in-game save succeeds, but saving
again over an existing save shows the same "Error saving data. The backup
memory has failed." screen.

## Root cause

Our FLASH `0x0A` PAGE_PROGRAM implemented textbook NOR semantics — AND-mask
against existing cells (`*slot &= byte`), on the assumption the game erases
pages first. The AUXSPI trace of a real HGSS save shows the SDK's flash
driver issues **only** WREN + PAGE_PROGRAM + RDSR + READ-verify — **no erase
commands ever** (no 0xDB page-erase, no 0xD8 sector-erase). On the real
cart's chip (the `FLASH_4MBITS_EX` IR-cart part), page-program evidently
writes cleanly over old data.

With AND semantics:

- Save 1 (blank 0xFF chip): `0xFF & data = data` — works. This is why the
  first-save verification passed.
- Save 2: HGSS's slot ping-pong writes the *other* (still blank) slot for
  the bulk data, **but re-programs the shared footer/counter pages written
  by save 1** — those AND-corrupt (`old & new`), the SDK's read-verify
  mismatches, and the game reports backup failure.
- Save 3+ would corrupt entire slots.

## Fix

`0x0A` now overwrites like `0x02` (comment documents the trace-derived
rationale). New unit test `test_flash_page_program_overwrites_existing_data`
covers program-over-existing-data (fails on the old AND behavior).

## Verification (2026-07-04)

- Two consecutive in-game saves from the New Bark Town state: save 1 →
  mirror slot (0x40000+) gets 130,708 non-FF bytes; save 2 → overwrite
  prompt ("There is already a saved file. Is it OK to overwrite?") → "Saving
  a lot of data…" → completes with **no error box**, and the primary slot
  (0x00000+) now also holds a clean 130,708-byte block. Exported image:
  `/private/tmp/hg-saved-double.sav`.
- The user's real .sav (containing footer pages corrupted by the pre-fix
  overwrite attempt) still boots: the game shows its designed recovery —
  "The save file is corrupted. The previous save file will be loaded." —
  and the CONTINUE menu loads the intact save-1 data. The message
  disappears after the next successful save.
- 667/667 nds-core tests pass.

## Notes

- We deliberately did NOT add 0xDB/0xD8 erase-command behavior changes:
  HGSS never issues them, and guessing their firing semantics (CS-release
  vs dummy byte) without a consumer risks breaking the one game family we
  can verify. The unhandled-command trace log will surface any title that
  needs them.

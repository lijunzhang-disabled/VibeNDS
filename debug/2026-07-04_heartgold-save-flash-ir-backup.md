# HeartGold "The backup memory has failed" — root cause: wrong backup chip + missing IR tunnel

Date: 2026-07-04
Status: **Fixed** (gamecode-based FLASH 512K + IR passthrough + WEL auto-clear)

## Symptom

In-game SAVE shows:

```
Error saving data.
The backup memory has failed.
The game may be played, but
it is impossible to save.
Please turn off the power.
```

## Root cause (three defects, all in AUXSPI backup emulation)

HGSS (US gamecode `IPKE`) uses a **512 KB FLASH** backup reached through the
cart's **infrared (IR) transceiver**. Our emulation got all three of those
wrong:

1. **Wrong chip type.** `BackupKind::guess_from_header` always returned
   `Eeprom64K` (64 KB, EEPROM command set). HGSS needs FLASH 512 KB. Two
   problems fall out: the game's `SaveDetectFlash` → `CARD_IdentifyBackup`
   probe uses the FLASH JEDEC-ID / status protocol, which an EEPROM chip
   doesn't answer; and even if writes were accepted, a 64 KB chip aliases
   the save-block mirrors (HGSS stores its two save slots at 0x00000 and
   0x40000 — both wrap to the same 64 KB, corrupting the ping-pong).

2. **Missing IR tunnel.** On `Ixxx` carts an IR chip sits between AUXSPI and
   the backup chip. **Every** backup transaction is prefixed with an IR
   command byte: `0x00` = "pass the rest of this CS-asserted transaction
   through to the backup chip", `0x08` = IR status probe (answers `0xAA`).
   Our state machine fed that leading `0x00` straight to the flash command
   decoder, which read it as a no-op/status command and then mis-framed the
   real command + address bytes. AUXSPI trace of a save attempt (with the
   fix's tunnel in place) shows the game's actual framing:
   `0x00, 0x06` (WREN), `0x00, 0x0A, addr…, data…` (page-program),
   `0x00, 0x05, 0x00` (RDSR) — i.e. a leading `0x00` on every transaction.

3. **WEL never auto-cleared.** Real FLASH/EEPROM clears the write-enable
   latch when a program/erase's internally-timed cycle begins. We only
   cleared WEL on the explicit `0xD8` sector-erase path; program (`0x02` /
   `0x0A`) left WEL set. HGSS's verify-after-write loop reads status and
   would see a stuck WEL, and the SDK's write bookkeeping depends on WEL
   dropping after each programmed page.

## Fix

`nds-core/src/cart/auxspi.rs`:

- `BackupKind::guess_from_gamecode(gamecode, device_capacity)` — maps the
  Pokémon gen 4/5 gamecodes (ADA/APA/CPU/IPK/IPG, IRB/IRA/IRE/IRD) to
  `Flash512K`, else falls back to the header heuristic.
- `BackupKind::is_ir_cart(gamecode)` — true for `Ixxx` gamecodes.
- New IR tunnel state machine (`IrPhase`): first byte of each CS-asserted
  transaction is the IR command; `0x00` → passthrough to the flash decoder,
  `0x08` → answer `0xAA`, anything else → ignore-and-echo. Reset on CS
  release (`end_transaction`).
- `end_transaction` now clears WEL when a program/write transaction closes.

Frontends (`harness.rs`, `main.rs`) call `guess_from_gamecode` and
`set_ir_cart(is_ir_cart(gamecode))`. Added a harness `export_save` command
to pull the backup image out for verification.

`nds-core/src/lib.rs`: `Nds::set_ir_cart`.

## Verification (done 2026-07-04)

Drove a real in-game save through the harness from the bedroom state
(regenerated on the current build): walked downstairs, through mom's
dialogue, opened the menu, tapped SAVE → YES.

- Bottom screen: "Would you like to save the game?" → "Saving a lot of
  data… Don't turn off the power." → **"AAAAAAA saved the game."** (no error
  box). Captures in `/private/tmp/hg-save-adaptive/` and `/private/tmp/hg-save-complete/`.
- AUXSPI trace: 544 FLASH page-programs, all correctly IR-tunneled.
- Exported backup: 512 KB, 130,708 non-0xFF bytes written across
  `0x40000..0x61a0f` — the full save-block slot, no wrap/alias.
- 673/673 workspace tests pass (5 new AUXSPI tests: gamecode guess + IR
  flag, IR passthrough tunneling, IR status probe, 512K high-address
  no-wrap, WEL auto-clear).

Negative control (old behavior) was not run as a fresh replay because the
fix adds fields to `AuxSpi`, so pre-fix binaries can't load the current
save-states; the user's error screenshot is the before-state, and the
mechanism is established directly from the decomp + AUXSPI trace.

## Game-side references (US HGSS, pret/pokeheartgold)

- `SaveDetectFlash` / `CARD_IdentifyBackup(CARD_BACKUP_TYPE_FLASH_4MBITS)` —
  4 Mbit = 512 KB is the expected chip (`src/save.c:1085`, SDK
  `CARD_IdentifyBackup` @ 0x020DD060).
- `FlashWriteChunkInternal` → `CARD_WriteAndVerifyBackupAsync` — program +
  read-back verify; a verify mismatch or stuck status loops forever
  ("Saving…" never completes), which is what a wrong chip / stuck WEL would
  cause.

## Follow-ups

- Backup-type selection is still a small gamecode allowlist. A proper fix is
  the public save-type database (gamecode → type/size); worth importing if
  more commercial titles are tested.
- IR model is minimal (passthrough + status probe). Games that actually use
  the IR link (e.g. gen-5 C-Gear / some minigames) will need real IR I/O.
- The JEDEC ID returned for FLASH is still a Macronix-style placeholder
  (`C2 11 05`). It satisfies HGSS's identify path; other titles that match on
  a specific manufacturer/type ID may need per-chip IDs.

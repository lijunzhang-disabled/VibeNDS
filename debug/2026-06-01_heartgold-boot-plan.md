# Pokemon HeartGold boot plan

Date: 2026-06-01
Status: **Plan**

## Goal

Boot `Pokemon-HeartGoldVersionUSA.nds` past early startup into visible game
output, then continue to title/menu and a first save-load smoke test.

The immediate target is not pixel-perfect gameplay. The first milestone is
simple: both display engines leave the all-black startup state and the ROM
reaches a recognizable boot/logo/title path without executing invalid code.

## Current Status

HeartGold does not boot yet.

The frontend command loads the ROM and save file, but both displays stay
disabled:

```sh
cargo run --release -p nds-frontend -- \
  --rom ~/Documents/Pokemon-HeartGoldVersionUSA.nds \
  --save-type flash-512k \
  --no-audio \
  --scale 2
```

Probe evidence from the current emulator:

- ROM header parses correctly: title `POKEMON HG`, gamecode `IPKE`.
- ARM9 direct-boot load/entry is sane: load `0x02000000`, entry `0x02000800`.
- ARM7 direct-boot load/entry is sane: load `0x02380000`, entry `0x02380000`.
- The game reaches a Slot-1 `B8` chip-ID command before enabling display.
- With the old fixed chip ID, ARM9 reached the SDK terminate loop around
  `0x020D3F64`.
- After making boot RAM and Slot-1 chip ID agree for the HeartGold IR/large
  cart shape, the clean terminate path moved, but ARM9 later escaped into
  invalid addresses while `DISPCNT_A/B` remained zero.

That means the current blocker is before graphics. The emulator is not yet
satisfying the retail gamecard protocol expected by this commercial ROM.

## Likely Missing Pieces

### 1. Retail Slot-1 protocol state

Current Slot-1 support is enough for homebrew/card examples:

- command register byte order;
- `ROMCTRL`/`CARD_DATA_RD` transfer status;
- header reads;
- plain chip-ID reads;
- plain `B7` ROM data reads.

HeartGold likely needs the retail protocol states:

- raw/pre-KEY1 mode;
- KEY1 command mode;
- KEY2 main-data mode;
- correct command-mode transitions after `0x3C`, `0x4...`, `0x1...`,
  `0x2...`, `0xA...`, then encrypted `0xB7`/`0xB8`.

GBATEK documents `B8` in main-data mode as a KEY2-encrypted command that
returns a KEY2-encrypted chip ID. Returning a plain chip ID is not enough once
the ROM expects encrypted transfer state.

### 2. KEY1 and KEY2

Implement or port:

- KEY1 command encryption/decryption from gamecode-derived keys;
- secure-area command handling;
- KEY2 stream setup from seed commands;
- KEY2 stream application to command bytes and returned data.

Do not hand-wave this with another fixed response. HeartGold is a good point
to add the real state machine because later commercial games will hit the same
class of failures.

### 3. Secure area behavior

Direct boot currently fakes the boot indicator secure-area result enough for
many tests. HeartGold may still perform runtime secure-area or card-state
checks.

Needed behavior:

- expose realistic boot indicator values;
- handle secure-area block reads in the expected command mode;
- return data with the right dummy periods and encryption state.

### 4. Backup type

Use flash save for HeartGold:

```sh
--save-type flash-512k
```

This is probably not the first black-screen blocker, because the ROM fails
before display setup. It will matter once the title/menu path starts probing or
writing save data.

### 5. Real BIOS/firmware path

HeartGold may eventually benefit from real BIOS and firmware dumps because the
hardware boot path naturally performs more cart setup than direct boot.

However, real BIOS support will not replace Slot-1 KEY1/KEY2. BIOS startup
also depends on those card operations working.

## Implementation Plan

### Step 1 — Instrument the card command sequence

Add a gated Slot-1 trace that records:

- command bytes;
- `ROMCTRL` value and transfer length;
- current card protocol mode;
- returned first few words;
- ARM9 PC around command issue/read.

Run HeartGold until failure and save the command sequence into this debug log.

### Step 2 — Move Slot-1 command handling into a protocol object

Create a small gamecard module/state machine instead of keeping all behavior in
`io_arm9.rs`.

Suggested state:

- loaded ROM bytes;
- current protocol mode: raw, KEY1, KEY2;
- KEY1/KEY2 seed/state;
- pending data FIFO;
- current chip ID.

Keep the existing homebrew behavior passing while adding structure for retail
commands.

### Step 3 — Implement chip ID consistently

Keep the shared chip-ID helper for:

- direct-boot indicators at `0x027FF800`, `0x027FF804`, `0x027FF860`;
- raw `0x90` chip-ID reads;
- base value encrypted for KEY2 `0xB8`.

HeartGold USA gamecode `IPKE` should use the IR/large-cart shape currently
captured by the helper test.

### Step 4 — Add KEY1 command support

Implement the KEY1 phase enough to handle:

- activate KEY1 encryption;
- activate KEY2 mode;
- second chip-ID / KEY2 stream command;
- secure-area block reads;
- enter main data mode.

Use GBATEK plus an established emulator implementation as the reference.

### Step 5 — Add KEY2 data-stream support

Implement KEY2 stream encryption/decryption for:

- `B7` main ROM data reads;
- `B8` chip-ID reads;
- dummy byte periods;
- stream advancement rules after invalid commands.

### Step 6 — Re-run milestones

Use this order after each change:

```sh
cargo test -p nds-core
```

Then quick ROM probes:

- `filesystem/nitrofs/nitrodir` still mounts and lists files.
- `card/eeprom` still identifies EEPROM.
- HeartGold frame probe no longer reaches invalid PC before display setup.
- Frontend command reaches visible output.

## Success Criteria

Milestone 1:

- HeartGold no longer reaches SDK terminate or invalid PC during the first
  120 frames.
- At least one display engine has nonzero `DISPCNT`.
- Framebuffer is non-black.

Milestone 2:

- Boot/logo/title screen is visible.
- Input reaches the next screen.

Milestone 3:

- Save type `flash-512k` can create/load a `.sav` without corrupting startup.

## Notes

`debug/test-plan.md` does not claim five commercial games already boot. It
defines that as the Stage 4 completion bar:

- five commercial titles boot to playable state;
- at least three complete a save round-trip.

The current verified broad-compatibility results are homebrew/devkitPro and
homebrew game candidates, not commercial-title passes.

# Test campaign — runtime compatibility sweep

Date: 2026-05-31
Status: **In progress**

## Symptom

After `armwrestler-fixed` was passing, the next compatibility sweep exposed
failures across older ARM7 tests, modern libnds startup, devkitPro examples,
filesystem/card examples, and homebrew games:

- `arm7wrestler` needed source-side build patches for modern devkitPro and
  then exposed ARM7 exception and instruction-edge bugs.
- devkitPro `hello_world`, `pxi`, `time`, and touch examples initially got
  stuck in startup or displayed blank frames.
- devkitPro graphics examples reached display setup but some 2D bitmap and
  3D paths produced empty or incorrect output.
- NitroFS and EEPROM card examples could not complete their card/probe
  paths.
- Early homebrew game candidates either stayed black or stalled before
  visible gameplay/menu screens.

## Investigation

The sweep used a mix of built test ROMs, temporary framebuffer probes, console
probes, audio probes, and source inspection:

- `arm7wrestler` was rebuilt under `/private/tmp/arm7wrestler` with local
  compatibility patches, then each menu page was captured after key presses.
- devkitPro examples under `/private/tmp/nds-examples` were smoke-tested with
  probes that recorded PC values, halt state, display registers, framebuffer
  nonzero counts, and console text.
- Graphics probes checked displayed framebuffers and the internal 3D
  framebuffer for `Simple_Tri`, `Simple_Quad`, bitmap BG samples, sprite
  samples, and rotation samples.
- Audio probes injected keys and counted active channels plus nonzero mixed
  PCM output for maxmod examples.
- Card/filesystem probes compared NitroFS, EEPROM, and libfat behavior to
  determine whether failures were direct Slot-1 reads, AUXSPI backup probing,
  or missing DLDI/FAT backing.
- Homebrew probes captured Flappy Bird DS, Spelunky DS, hbmenu `argvTest`,
  cellsDS, and neo-engine output. Source inspection was used when Spelunky
  reached a menu but did not respond to the first guessed `A` input sequence.

## Root Causes

The failures were not one subsystem. The current sweep found several layers
that were each good enough for `armwrestler-fixed` but not for broader
homebrew:

1. **ARM7 exception/direct-boot state was incomplete.**
   `arm7wrestler` expected an Undefined-mode stack and old libnds-style
   undefined-handler slot behavior.

2. **Some ARM instruction edge cases still differed by core.**
   ARM7 `LDM` writeback, CP14/CP15 behavior, and ARM7 DSP multiply handling
   needed behavior distinct from ARM9.

3. **Modern libnds/calico startup needs more no-BIOS runtime behavior.**
   Startup paths use boot-detection SWIs, debugger-detection SWIs, synthetic
   IRQ vectors, CP15 WFI, CP15 c13 scratch state, ARM7 HALTCNT, and IPC FIFO
   send-empty IRQ behavior before user code can draw anything.

4. **Direct boot needed more accurate memory and argv setup.**
   ARM7 WRAM mirrors could accidentally alias private WRAM; modern Calico
   could also observe stale NDS header bytes as a non-null argv header.

5. **Graphics examples depended on missing hardware blocks.**
   The devkitPro 3D examples needed ARM9 divide/sqrt registers and corrected
   GPU3D matrix conventions. Bitmap samples needed mode 3-5 affine bitmap BG
   rendering. Homebrew also exposed Engine B BG/OBJ extended palette use.

6. **Card examples needed both Slot-1 and AUXSPI details.**
   NitroFS needed direct Slot-1 command support, ROM data reads, and EXMEMCNT
   ownership behavior. The EEPROM example needed AUXSPI transaction reset on
   chip deselect and old-libnds-friendly EEPROM identification behavior.

7. **libfat is a separate DLDI-backed block-device problem.**
   The current emulator can satisfy NitroFS and EEPROM paths, but
   `filesystem/libfat/libfatdir` still has only a DLDI stub and no mounted FAT
   block device. That requires real DLDI/FAT backing, not another Slot-1
   command tweak.

## Fix

Implemented during this sweep:

- ARM7 direct-boot Undefined-mode stack initialization and no-BIOS undefined
  vector behavior, including old handler-slot dispatch and safe return when
  no handler is installed.
- ARM7-specific instruction behavior for `LDM` base writeback, CP14 no-op
  reads, CP15 undefined handling, and DSP multiply no-op expectations from
  `arm7wrestler`.
- Additional no-BIOS SWI coverage for libnds startup paths, including boot
  source detection, debugger detection, halt/sleep/delay-style calls, divide,
  and sqrt.
- CP15 WFI parking, CP15 c13 round-trip state, high-vector handling, and
  direct-boot IRQ fallback behavior for modern and old libnds layouts.
- ARM exception return fixes so `MOVS PC,LR` and `LDM...PC^` preserve the
  restored SPSR Thumb bit.
- IPC FIFO send-empty IRQ delivery, ARM7 HALTCNT halt requests, ARM7 WRAM
  mirror routing, and direct-boot argv cleanup.
- ARM9 hardware divide/sqrt registers at `0x04000280..0x040002BF`.
- GPU3D row-vector/row-major matrix behavior and removal of the incorrect
  global 3D-enable interpretation of `DISP3DCNT` bit 0.
- GPU2D mode 3-5 affine bitmap BG rendering and Engine B BG/OBJ extended
  palette rendering.
- Minimal Slot-1 ROM command support for header/chip-ID/main-data reads,
  command register byte order, `ROMCTRL`, `CARD_DATA_RD`, and `EXMEMCNT`.
- AUXSPI transaction reset on deselect and EEPROM identification/status
  behavior compatible with the devkitPro EEPROM sample.

## Regression Tests

The unit suite grew to cover the hardware behavior added in this sweep,
including:

- ARM7 direct-boot/vector behavior and ARM7-specific instruction semantics.
- ARM exception return Thumb-state preservation.
- IPC send-empty IRQ behavior.
- ARM9 hardware divide/sqrt register behavior.
- Slot-1 command byte order, data read offsets, transfer-status clearing,
  and EXMEMCNT ownership.
- AUXSPI deselect/reset and EEPROM probe behavior.
- Engine B BG/OBJ extended palette rendering through VRAM H/I.
- Direct-boot ARM9 IRQ fallback when no libnds handler is installed yet.

## Verification

Current core regression suite:

```sh
cargo test -p nds-core
```

Latest recorded result: `323 passed; 0 failed`.

Manual/probe evidence gathered during the sweep:

- `arm7wrestler`: all seven pages render `OK` rows on the locally patched
  build: `ARM ALU`, `ARM LDR/STR`, `ARM LDM/STM`, `THUMB ALU`,
  `THUMB LDR/STR`, `THUMB LDM/STM`, and `ARM V5TE`.
- devkitPro `hello_world`, `pxi`, `time/*`, and touch examples reach visible
  display/console startup and settle into expected wait paths.
- devkitPro graphics smoke passes for representative bitmap BG, rotation,
  sprite, and simple 3D examples. `Simple_Tri` and `Simple_Quad` produce
  nonzero internal and displayed 3D framebuffers.
- maxmod audio examples produce active channels and nonzero mixed PCM after
  injected input.
- `filesystem/nitrofs/nitrodir` mounts NitroFS and lists embedded files.
- `card/eeprom` opens Slot-1, identifies EEPROM, and reads erased backup
  bytes after injected input.
- hbmenu `argvTest`, cellsDS, neo-engine, Flappy Bird DS, and Spelunky DS now
  reach visible startup/title/menu screens. Flappy also advances through a
  touch/A-driven gameplay loop to a game-over screen.

## Known Remaining Work

- `filesystem/libfat/libfatdir` still reports
  `fatInitDefault failure: terminating` because there is no real DLDI/FAT
  block-device backing yet.
- Spelunky DS reaches the title/menu screen, but the correct interaction
  sequence for entering gameplay still needs to be driven and verified.
- Graphics results are smoke-level compatibility checks, not pixel-perfect
  comparisons against hardware or melonDS.

## Lessons

The useful order for this phase is still CPU tests first, then focused
devkitPro examples, then broader homebrew. Each layer made the next failure
more specific: once CPU/direct-boot issues were fixed, libnds startup exposed
IPC and no-BIOS runtime gaps; once startup worked, graphics/card/audio probes
became meaningful; once those worked, homebrew games exposed palette and
old-libnds IRQ assumptions.

For filesystem work, keep NitroFS/card access separate from libfat/DLDI.
NitroFS proves Slot-1 ROM reads; EEPROM proves AUXSPI backup access; libfat
requires a mounted FAT block device and a patched DLDI interface.

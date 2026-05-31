# Daily debugging summary

Date: 2026-05-31
Status: **Carryover**

## Scope

Today's work started from the `armwrestler-fixed` black-screen failure and
expanded into a broader NDS compatibility sweep across CPU tests, modern
libnds/devkitPro examples, graphics, audio, card access, filesystem tests, and
early homebrew candidates.

## Completed

- Brought `armwrestler-fixed` from black screen to a full visual pass.
- Added direct-boot support needed by homebrew that loads ARM7 code into
  private ARM7 WRAM.
- Fixed ARM/Thumb CPU edge cases exposed by `armwrestler-fixed` and
  `arm7wrestler`, including shifted-register decode, unaligned word loads,
  `LDM` base writeback, exception return state, and ARM7-specific CP/DSP
  behavior.
- Added enough no-BIOS/libnds runtime behavior for modern devkitPro examples
  to reach user code: startup SWIs, IRQ fallback paths, IPC FIFO behavior,
  ARM7 halt handling, CP15 WFI/c13 state, and direct-boot argv cleanup.
- Implemented hardware behavior needed by the sweep, including ARM9
  divide/sqrt registers, Slot-1 ROM reads, AUXSPI EEPROM probing, Engine B
  extended palettes, affine bitmap backgrounds, and 3D matrix fixes.
- Verified representative devkitPro examples for console startup, PXI/time,
  touch input, graphics, maxmod audio, NitroFS, and EEPROM behavior.
- Verified several homebrew candidates reach visible startup/title/menu
  screens, including hbmenu `argvTest`, cellsDS, neo-engine, Flappy Bird DS,
  and Spelunky DS.

## Current Work In Progress

The current unmerged runtime work adds an initial emulator-backed DLDI/PXI
block-device service:

- Calico block-device channel 2 request handling for DLDI presence, init,
  sector reads, and sector writes.
- A lazily initialized synthetic FAT16 image with a small root directory.
- Unit coverage for sector-count exposure and boot-sector reads.

This is not yet a ROM-level libfat pass. The devkitPro
`filesystem/libfat/libfatdir` ROM still reports:

```text
fatInitDefault failure: terminating
```

The next debugging step is to determine whether the ROM reaches the ARM9 PXI
DLDI service at all. A temporary console probe under `/private/tmp` was being
updated to print the emulator DLDI image length after running `libfatdir`; if
that length stays zero, libfat is rejecting the stub before the PXI path. If it
becomes nonzero, the synthetic FAT image or request handling is the likely
problem.

## Tests Run

Recorded checks during the day included:

```sh
cargo test
cargo test -p nds-core
cargo test -p nds-core test_direct_boot_arm9_irq
```

Latest recorded result for `cargo test -p nds-core`: `326 passed; 0 failed`.

## Carryover

- Finish ROM-level `filesystem/libfat/libfatdir` support.
- Drive Spelunky DS past the title/menu screen with the correct interaction
  sequence.
- Convert the current smoke-level graphics and homebrew checks into a more
  repeatable compatibility test script or matrix.
- Rerun the full core regression suite after the DLDI/libfat result changes.

## Notes

The most useful pattern today was stepping outward by layer: CPU diagnostics
first, then libnds startup, then graphics/card/audio examples, then homebrew.
Each layer removed enough noise for the next failure to become specific.

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

## Current Filesystem Result

The runtime work adds an emulator-backed DLDI/PXI block-device service:

- Calico block-device channel 2 request handling for DLDI presence, init,
  sector reads, and sector writes.
- A lazily initialized synthetic FAT16 image with a small root directory.
- Unit coverage for sector-count exposure and boot-sector reads.

This now reaches a ROM-level libfat pass. The devkitPro
`filesystem/libfat/libfatdir` ROM mounts the emulator-backed FAT16 volume and
lists the synthetic root directory entries `README.TXT` and `[GAMES]`.

The failure was in the synthetic boot sector, not PXI request delivery:
`dldi_len=16777216` proved the ROM had reached the DLDI service. libfat's
FAT12/16 VBR probe expected the 16-bit total-sector BPB field to be populated
for the 32K-sector volume; using only the 32-bit total-sector field caused
`fatInitDefault failure: terminating`.

## Tests Run

Recorded checks during the day included:

```sh
cargo test
cargo test -p nds-core
cargo test -p nds-core test_direct_boot_arm9_irq
```

Latest recorded result for `cargo test -p nds-core`: `326 passed; 0 failed`.

## Carryover

- Convert the current smoke-level graphics and homebrew checks into a more
  repeatable compatibility test script or matrix.
- Rerun the full core regression suite after the next runtime result changes.

## Notes

The most useful pattern today was stepping outward by layer: CPU diagnostics
first, then libnds startup, then graphics/card/audio examples, then homebrew.
Each layer removed enough noise for the next failure to become specific.

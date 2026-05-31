# NDS Emulator — A Rust Nintendo DS Emulator

A Nintendo DS emulator built from scratch in Rust, sibling to the GBA
emulator at `../gba`. This project follows the same AI-human pair
programming approach: the human steers design and compatibility priorities,
while the AI handles much of the implementation and debugging.

```
┌──────────────────────────────────────┐
│  ARM946E-S + ARM7TDMI CPUs           │ ← ARM + THUMB, direct boot support
│        +                              │
│  Shared bus + memory map              │ ← Main RAM, WRAM, VRAM, OAM,
│        +                              │    palette, cart, firmware, saves
│  2D engines + partial 3D pipeline     │ ← dual 256×192 screens
│        +                              │
│  Audio, DMA, timers, IPC, interrupts  │
└──────────────────────────────────────┘
```

## Why this exists

This is a low-level systems project and a compatibility testbed for building
emulators with AI-assisted development. The `debug/` directory records real
bug investigations as symptom → root cause → fix → regression test, so the
project history is useful rather than just a pile of patches.

## Run a ROM

```sh
cargo build --release
cargo run --release -p nds-frontend -- --rom path/to/game.nds
```

Useful flags:

```text
--bios-arm9 PATH    Use a real ARM9 BIOS dump
--bios-arm7 PATH    Use a real ARM7 BIOS dump
--firmware  PATH    Use a real firmware dump
--save-type TYPE    Force backup type
--no-audio          Disable audio output
--scale N           Window scale factor
```

## Project Docs

| Doc | What it is for |
|---|---|
| [`PLAN.md`](PLAN.md) | Phase plan and implementation roadmap |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Core design for CPUs, bus, GPU, audio, scheduler, saves |
| [`debug/`](debug/) | Bug investigations and test-ROM debugging notes |
| [`docs/concepts/`](docs/concepts/) | Short concept notes for emulator subsystems |

## Status

Core emulator systems are in active development. Current coverage includes
dual CPUs, memory/bus routing, DMA, timers, interrupts, IPC, cartridge saves,
2D rendering, partial 3D rendering, audio plumbing, firmware/SPI, direct
boot, and save states.

### Test ROM Compatibility

| Test ROM | Status |
|---|---|
| [`Atem2069/armwrestler-fixed`](https://github.com/Atem2069/armwrestler-fixed) via `test-roms/armwrestler.nds` | Passes via direct boot |

Run the Armwrestler check with:

```sh
cargo run --release -p nds-frontend -- --rom test-roms/armwrestler.nds --no-audio --scale 1
```

The matching debugging writeup is
[`debug/2026-05-31_armwrestler-fixed-direct-boot.md`](debug/2026-05-31_armwrestler-fixed-direct-boot.md).

## Reference

- [GBATEK](https://problemkaputt.de/gbatek.htm) — Nintendo DS hardware reference
- [Atem2069/armwrestler-fixed](https://github.com/Atem2069/armwrestler-fixed) — ARM instruction test ROM

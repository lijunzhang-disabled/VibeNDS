# VibeNDS — An AI-Coded Nintendo DS Emulator in Rust

A Nintendo DS emulator built from scratch in Rust, sibling to [VibeGBA](https://github.com/lijunzhang-disabled/VibeGBA) and built the same way: AI-human pair programming. The human steered the direction and made design calls; the AI handled most of the implementation and debugging — both CPUs, the shared bus, the 2D and 3D graphics pipelines, audio, DMA, cart and save-media emulation, and the tooling used to debug real games.

```
┌────────────────────────────────────────┐
│  ARM946E-S (67 MHz) + ARM7TDMI (33 MHz)│ ← ARM + THUMB + v5TE, lockstep,
│        +                                │   direct boot, BIOS HLE
│  Shared bus + memory map                │ ← Main RAM, WRAM, 9 VRAM banks,
│        +                                │   OAM, palette, IPC, cart, saves
│  2×2D engines + 3D pipeline             │ ← dual 256×192 screens; geometry
│        +                                │   engine + software rasterizer
│  APU (16 ch), DMA, timers, interrupts   │ ← PCM8/16, ADPCM, PSG, noise
└────────────────────────────────────────┘
```

## Why this exists

Same experiment as VibeGBA, one console generation harder: two CPUs in lockstep, a geometry engine feeding a scanline rasterizer, nine remappable VRAM banks, and commercial games that lean on all of it. The interesting question isn't just "can AI write an emulator" but "can AI *debug* one" — chasing a wrong texture or a failed save through traces, RAM dumps, and disassembly of the game itself.

The [`debug/`](debug/) directory is the answer in long form: 17+ investigation writeups, each walking symptom → hypotheses → root cause → fix → verification. Highlights include a texture-streaming bug that traced back to cartridge transfer *timing* overflowing the game's internal task queue, and a save-file failure that required emulating the infrared chip HeartGold hides between the SPI bus and its flash memory.

## Run a game

```bash
# Build (bundled SDL2 needs CMake; the required CMake policy flag is
# pre-configured in .cargo/config.toml)
cargo build --release

# Play
./target/release/nds-frontend --rom path/to/game.nds
```

Useful flags:

```text
--bios-arm9 PATH    Use a real ARM9 BIOS dump (optional; BIOS HLE otherwise)
--bios-arm7 PATH    Use a real ARM7 BIOS dump
--firmware  PATH    Use a real firmware dump
--save-type TYPE    Force backup type (auto-detected from gamecode otherwise)
--no-audio          Disable audio output
--scale N           Window scale factor
--harness           Machine-drivable JSON protocol on stdio (see below)
```

Saves are read from and written to `<rom>.sav` next to the ROM. F5/F8 save and load full emulator save states.

## Project docs

| Doc | What it's for |
|---|---|
| [`PLAN.md`](PLAN.md) | Phase-by-phase implementation plan with status |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Technical deep-dive: CPUs, bus ownership model, VRAM routing, 3D pipeline, scheduler |
| [`debug/`](debug/) | Bug investigations — symptom → root cause → fix → verification |
| [`docs/concepts/`](docs/concepts/) | Short concept notes on DS subsystems (GPU command flow, IPC, SPI, rasterization…) |

## Status

- Phases 1–8 (CPUs, bus, 2D engines, DMA/timers/IPC, SPI + saves, 3D geometry, 3D rasterizer, audio + save states) — **Done**
- Phase 9 (accuracy polish) — ongoing, driven by real-game debugging

674 checked-in unit tests, ~35,000 lines of Rust across `nds-core` and `nds-frontend`.

### Test ROM compatibility

[armwrestler-fixed](https://github.com/Atem2069/armwrestler-fixed) (ARM9) and ARM7Wrestler (Mic, 2006) — CPU instruction validation on both cores:

| ROM | Status |
|---|---|
| `armwrestler.nds` (ARM9: ALU, LDR/STR, LDM/STM, THUMB, v5TE) | ✅ ALL PASS |
| `arm7wrestler.nds` (ARM7: ALU, LDR/STR, LDM/STM, THUMB) | ✅ ALL PASS |

Both run via direct boot. The matching writeup is [`debug/2026-05-31_armwrestler-fixed-direct-boot.md`](debug/2026-05-31_armwrestler-fixed-direct-boot.md).

Test ROMs are not bundled — drop them into `test-roms/` (gitignored) and run them with `--rom`.

### Playable games (spot-tested)

| Game | Status |
|---|---|
| Pokémon HeartGold (US) | Playable through the early game: intro cinematics, dialogue, overworld with 3D billboard sprites, menus, in-game saving — including save-overwrite and continue-from-save |

Getting HeartGold from black screen to playable-and-saving drove most of Phase 9 so far; every fix along the way is documented in [`debug/`](debug/).

## How it was built

Development followed a phased plan (documented in [`PLAN.md`](PLAN.md)). AI generated the bulk of the code, with human review and course-correction throughout:

1. **Workspace + dual CPU cores + bus skeleton** — ARM946E-S and ARM7TDMI (ARM + THUMB + v5TE), lockstep scheduling, per-CPU buses over shared state
2. **Cart loader + boot + scheduler + IRQs** — direct boot, cycle-based event scheduler, both interrupt controllers
3. **2D Engines A/B + VRAM routing** — text/affine/extended BGs, sprites, windows, blending, the 9-bank VRAMCNT router
4. **DMA + timers + IPC + input** — 4+4 DMA channels with NDS start modes, IPC FIFO/Sync between the CPUs
5. **SPI + AUXSPI backup** — firmware, touchscreen, power management; EEPROM/FRAM/FLASH backup chips, IR-cart tunneling
6. **3D geometry pipeline** — GXFIFO, matrix stacks, lighting, clipping, viewport transform
7. **3D rasterizer** — scanline rendering with perspective-correct textures, depth/alpha/fog/edge-marking, display capture
8. **Audio + save states** — 16-channel mixer (PCM8/16, IMA-ADPCM, PSG, noise), zstd-compressed full-machine save states
9. **Accuracy polish** — ongoing; real-game conformance work, each fix documented in `debug/`

### AI-driven debugging

The distinctive tooling in this repo is what the AI uses to debug games autonomously:

- **`--harness` mode** — a JSON-over-stdio protocol for frame-exact stepping, input injection, save states, RAM/VRAM/texture dumps, and 3D pipeline introspection, so an agent can drive the emulator, take screenshots, and inspect state without a human clicking through
- **`NDS_TRACE_*` env hooks** — targeted tracing (exec at chosen PCs with full register dumps, DMA/VRAM/SPI traffic, cart transfers) that stays zero-cost when off; 3D debug capture gates behind `NDS_DEBUG_3D=1`
- **Game-side reverse engineering** — investigations cross-reference the [pret/pokeheartgold](https://github.com/pret/pokeheartgold) decompilation to trace bugs *into the game's own code* (e.g. proving a texture upload was dropped because the game's 32-slot VBlank task queue silently overflowed when our cart reads completed too fast)

## Reference

- [GBATEK](https://problemkaputt.de/gbatek.htm) — the definitive GBA/NDS hardware spec
- [armwrestler-fixed](https://github.com/Atem2069/armwrestler-fixed) — ARM9 instruction test ROM
- [pret/pokeheartgold](https://github.com/pret/pokeheartgold) — HeartGold decompilation, invaluable for game-side debugging
- [emudev.org Discord Resources](https://github.com/emudev-org/discord-resources) — community-curated emulator development resources

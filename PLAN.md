# NDS Emulator — Implementation Plan

## Overview

Building a Nintendo DS emulator from scratch in Rust. Dual-CPU (ARM946E-S + ARM7TDMI), dual 256×192 screens, 2D engines A/B with VRAM bank routing, 3D GPU (geometry + rasterizer), SDL2 frontend. Sibling project to `../gba`; we mirror its workspace shape, planning style, and testing discipline.

GBATEK (https://problemkaputt.de/gbatek.htm) is the primary reference. Where GBATEK is ambiguous, we cross-check against the melonDS source and on-hardware test cases (NDS test programs in homebrew).

## Current Status

| Phase | Status | Tests |
|---|---|---|
| Phase 1: Workspace + ARM9/ARM7 cores + dual bus skeleton | **Done** | 62 |
| Phase 2: Cart loader + boot transfer + scheduler + IRQs | **Done** | 89 |
| Phase 3: 2D Engine A + Engine B + VRAM bank routing | **Done** | 104 |
| Phase 4: DMA + timers + IPC FIFO/Sync + keypad/EXTKEYIN | **Done** | 137 |
| Phase 5: SPI (firmware + TSC + PMIC) + AUXSPI cart backup | **Done** | 161 |
| Phase 6: 3D geometry pipeline (matrix stacks, vertex, clip, GXFIFO) | **Done** | 227 |
| Phase 7: 3D rasterizer (depth, alpha, edge, fog, anti-alias, capture) | Not started | — |
| Phase 8: Audio (16 ch, capture units) + save states + AUXSPI export | Not started | — |
| Phase 9: Accuracy polish + debugger utilities | Not started | — |

WiFi, RTC alarm, GBA-slot passthrough, and DS-Wireless multiplay are deliberately out of scope. RTC time-of-day **is** in scope (most cart games read it on boot).

## NDS Hardware Summary

| Component | Details |
|---|---|
| ARM9 | ARM946E-S, ARMv5TE, 67.027964 MHz, has CP15 (MPU + TCM + cache control) |
| ARM9 caches | 8 KB I-cache, 4 KB D-cache (4-way set associative, 32 B lines) |
| ARM9 TCM | 32 KB ITCM (mirrored to fit address window), 16 KB DTCM (relocatable) |
| ARM7 | ARM7TDMI, ARMv4T, 33.513982 MHz (½ of ARM9) |
| Main RAM | 4 MB shared, accessed by both CPUs via the main-bus arbiter |
| ARM7 WRAM | 64 KB, ARM7-only |
| Shared WRAM | 32 KB, banked between ARM7/ARM9 via `WRAMCNT` (4 modes) |
| Display | 2 × 256×192, BGR555, 59.8261 Hz, top + bottom |
| 2D engines | Engine A (full feature set, all VRAM, ext-OAM/palette), Engine B (subset, banks C/H/I only) |
| 3D engine | GXFIFO command pipeline, 4-deep MTX_PUSH/POP stacks, hardware rasterizer with up to 6144 vertices / 2048 polygons per frame |
| Sound | 16 hardware channels (PCM8/PCM16/IMA-ADPCM/PSG square + noise), 2 capture units, ARM7-side |
| DMA | 4 channels on ARM9 + 4 on ARM7, with NDS-only start timings (GXFIFO, scanline, slot-1, slot-2) |
| Timers | 4 per CPU (8 total), prescaler + cascade |
| Cart | NDS slot-1 (encrypted protocol w/ KEY1/KEY2 + AUXSPI backup) + GBA slot-2 |
| Input | KEYINPUT (10 buttons) + EXTKEYIN (X/Y/touch/lid/debug) on ARM7 + 12-bit ADC TSC over SPI |
| BIOS | ARM9 BIOS = 4 KB (vector table + SWIs), ARM7 BIOS = 16 KB (also includes RSA + GBA fallback) |
| Firmware | 256 KB SPI flash: header, user settings (twice), WiFi calibration, optional boot menu code |

### Timing Constants (ARM7 clock domain)

We track the global cycle counter in **ARM7 cycles** since they're the lowest-common-multiple frequency of the two CPUs (every ARM7 cycle = 2 ARM9 cycles).

- Cycles/dot: 6 (ARM7) / 12 (ARM9)
- Dots/line: 355 (256 visible + 99 HBlank)
- Cycles/line: 2130 (ARM7)
- Lines/frame: 263 (192 visible + 71 VBlank)
- Cycles/frame: 560,190 (ARM7) ≈ 1,120,380 (ARM9)
- Frame rate: 59.8261 Hz

### ARM9 Memory Map

| Address Range | Size | Region |
|---|---|---|
| 0x00000000-0x00007FFF | 16-32 KB | ITCM (mirrored across the configured window) |
| 0x01FF8000-0x01FFFFFF | up to 16 KB | DTCM (base + size set in CP15 c9; address shown is BIOS default) |
| 0x02000000-0x02FFFFFF | 4 MB (mirrored) | Main RAM |
| 0x03000000-0x037FFFFF | 0/16/32 KB | Shared WRAM (mapping per WRAMCNT) |
| 0x04000000-0x040007FF | 2 KB | ARM9 I/O (DISPCNT_A, BG/OAM regs, DMA9, TIMER9, IPC, GX, VRAM control) |
| 0x04001000-0x040010FF | 256 B | Engine B I/O (DISPCNT_B + BG/OBJ for engine B) |
| 0x05000000-0x050007FF | 2 KB | Palette RAM (1 KB Engine A + 1 KB Engine B) |
| 0x06000000-0x061FFFFF | up to 656 KB | VRAM (banks A-I routed via VRAMCNT_A..I) |
| 0x06800000-0x068A3FFF | 656 KB | VRAM raw (banks A-I as a flat region for LCDC mode debug) |
| 0x07000000-0x070007FF | 2 KB | OAM (1 KB Engine A + 1 KB Engine B) |
| 0x08000000-0x09FFFFFF | 32 MB | GBA slot-2 ROM (when EXMEMCNT routes to ARM9) |
| 0x0A000000-0x0A00FFFF | 64 KB | GBA slot-2 SRAM |
| 0xFFFF0000-0xFFFF0FFF | 4 KB | ARM9 BIOS (high vector table, fixed) |

### ARM7 Memory Map

| Address Range | Size | Region |
|---|---|---|
| 0x00000000-0x00003FFF | 16 KB | ARM7 BIOS |
| 0x02000000-0x02FFFFFF | 4 MB | Main RAM (same mirror as ARM9) |
| 0x03000000-0x037FFFFF | 0/16/32 KB | Shared WRAM (per WRAMCNT) — falls through to ARM7 WRAM if unmapped |
| 0x03800000-0x0380FFFF | 64 KB | ARM7 WRAM |
| 0x04000000-0x040007FF | 2 KB | ARM7 I/O (sound, SPI, RTC, AUXSPI, IPC, DMA7, TIMER7, WiFi stubs) |
| 0x04800000-0x048FFFFF | — | WiFi MAC region (stubbed) |
| 0x06000000-0x06FFFFFF | up to 256 KB | VRAM access (banks C/D when allocated to ARM7) |
| 0x08000000-0x09FFFFFF | 32 MB | GBA slot-2 ROM (when EXMEMCNT routes to ARM7 — default) |
| 0x0A000000-0x0A00FFFF | 64 KB | GBA slot-2 SRAM |

### VRAM Bank Layout

Nine banks routed independently via `VRAMCNT_A..I` (offset 0x04000240..0x04000249, plus `WRAMCNT` at 0x04000247).

| Bank | Size | Common targets |
|---|---|---|
| A | 128 KB | LCDC, Engine A BG, Engine A OBJ, texture image |
| B | 128 KB | LCDC, Engine A BG, Engine A OBJ, texture image |
| C | 128 KB | LCDC, Engine A BG, ARM7, texture image, Engine B BG |
| D | 128 KB | LCDC, Engine A BG, ARM7, texture image, Engine B OBJ |
| E | 64 KB | LCDC, Engine A BG, Engine A OBJ, texture palette, Engine A BG ext-pal |
| F | 16 KB | LCDC, Engine A BG, Engine A OBJ, texture palette, BG/OBJ ext-pal |
| G | 16 KB | LCDC, Engine A BG, Engine A OBJ, texture palette, BG/OBJ ext-pal |
| H | 32 KB | LCDC, Engine B BG, Engine B BG ext-pal |
| I | 16 KB | LCDC, Engine B BG, Engine B OBJ, Engine B OBJ ext-pal |

Each `VRAMCNT_x` byte: `[7]=enable, [6:5]=offset, [4:3]=reserved, [2:0]=mst (mode select)`. The same physical bank can be visible to both Engine A and the 3D texture unit simultaneously, but never to two CPU-side regions at once.

## Implementation Phases

### Phase 1: Workspace + Dual CPU Cores + Bus Skeleton

**Goal**: ARM9 and ARM7 each step instructions against their own bus; main RAM, TCMs, ARM7 WRAM, shared WRAM, and BIOS reads work; CP15 controls TCM remap.

**Plan:**
- Cargo workspace: `nds-core` (library, no platform deps) + `nds-frontend` (SDL2 binary).
- **CPU core (`cpu/`)** — port the GBA's `arm7tdmi` and add ARMv5TE: `CLZ`, `BLX` (immediate + register), `QADD`/`QSUB`/`QDADD`/`QDSUB`, `SMLA<x><y>` / `SMLAW<y>` / `SMLAL<x><y>` / `SMUL<x><y>` / `SMULW<y>`, `LDRD`/`STRD`, `MCR`/`MRC`/`CDP`. Gate the additions behind a `is_arm9: bool` flag so the same struct serves both cores. Keep separate `Cpu` instances for ARM9 and ARM7.
- **CP15** (`cpu/cp15.rs`) — system control coprocessor for ARM9: control register (cache enable, alignment fault, vector base = 0xFFFF0000), MPU regions (8 entries × {base, size, access perms}), ITCM/DTCM base+size registers (c9, c1), cache-clean-invalidate ops (c7, mostly NOPs in our impl). MPU is a stub at first; only TCM remap is functional.
- **Bus** (`bus/arm9.rs`, `bus/arm7.rs`) — separate per-CPU bus structs, each owning its CPU-only regions. Shared regions (Main RAM, Shared WRAM, palette/VRAM/OAM, IPC, IRQ flags) live in a `SharedState` borrowed by both. We use the GBA's "sibling fields" trick scaled up: top-level `Nds` owns `cpu9`, `cpu7`, `shared`, plus per-CPU peripherals. See `ARCHITECTURE.md` for the borrow-checker plan.
- **Memory mapping** — address-decode by `addr >> 24`, with sub-decoding inside the I/O page. Open-bus behavior on unmapped reads (per CPU, per side).
- **Scheduler** — port from GBA, but the timestamp counter is in ARM7 cycles; ARM9 ticks consume 0.5 cycles per cycle (we run ARM9 for 2 instructions per ARM7 instruction at the high level, refining later).
- **SDL2 frontend skeleton**: dual-window or single-window-with-stacked-screens, no rendering yet (clears to black).

**Tests planned:**
- ARMv5TE encode/decode round-trip for the new instructions.
- TCM remap: write through CP15 c9, see ITCM at the new base.
- WRAMCNT mode switching: shared WRAM visible to ARM9 in mode 0, to ARM7 in mode 3, both halves split in modes 1/2.

### Phase 2: Cart Loader + Boot Transfer + Scheduler Wiring

**Goal**: Load a `.nds` ROM, parse the header, perform the BIOS-equivalent "direct boot" transfer of ARM9/ARM7 binaries into RAM, set up entry points and stack pointers, run V/HBlank events.

**Plan:**
- **Cart parser** (`cart/header.rs`) — full 0x200-byte header: title, gamecode, ARM9/ARM7 ROM offset/entry/load/size, FNT/FAT offsets, banner offset, KEY1 area, secure area marker, ROM control flags, header CRC.
- **Direct boot** (`cart/direct_boot.rs`) — emulate the firmware's RAM-loader stage: copy ARM9 binary to its load address (typically 0x02000000), ARM7 binary to its load address (typically 0x02380000 or 0x037F8000), write the standard set of "boot indicator" values to Main RAM (chip ID at 0x027FF800, header copy at 0x027FFE00, etc.), set ARM9 PC = ARM9_entry, ARM7 PC = ARM7_entry, set both stack pointers per BIOS convention.
- **BIOS HLE stubs** — at minimum `SWI 0x06` (Div), `SWI 0x09` (Sqrt), `SWI 0x0B` (CpuSet), `SWI 0x0C` (CpuFastSet), `SWI 0x04` (IntrWait), `SWI 0x05` (VBlankIntrWait), `SWI 0x14`-style decompressors. ARM9 and ARM7 BIOS HLE tables are separate (different SWI numbering for some entries).
- **IRQ controller** — per-CPU `IE`/`IF`/`IME` (each is a `u32`, 22+ sources on each side). Wire VBlank, HBlank, VCount, IPC FIFO send/recv, IPC sync, slot-1, slot-2.
- **Scheduler events**: `HBlank`, `HBlankEnd`, `VBlank`, `TimerOverflow{cpu, id}`, `DmaComplete{cpu, ch}`, `GxFifo`, `Slot1Done`, `AuxSpiDone`, `AudioSample`.

**Tests planned:**
- Header parse + CRC validation against a known-good ROM dump.
- Direct boot: after running 0 cycles, verify Main RAM contains the ARM9 binary at the expected address.
- VBlank firing: with both CPUs in halt, after 560,190 ARM7 cycles, the VBlank flag is set on both CPUs' `IF`.

### Phase 3: 2D Engine A + Engine B + VRAM Routing

**Goal**: Render real DS games' 2D output. Both engines, all 7 BG modes (0-6), text + affine + extended modes (256×16 / 256-color affine bitmap / direct-color affine bitmap), OBJ rendering with 1D/2D mapping and ext-OAM 1024-entry mode, full layer compositing with windows/blending, VRAM bank routing.

**Plan:**
- **VRAM controller** (`vram.rs`) — for each bank, given `VRAMCNT_x`, compute the (target, address-within-target) mapping. Maintain a per-target view (Engine A BG slots, Engine A OBJ slots, Engine B BG/OBJ, ARM7 region, texture image, texture palette, BG/OBJ ext-pal). Multiple banks can coexist in one target (writes go to all mapped banks; reads pick the highest-priority bank — or we can panic-on-overlap during dev to catch bugs).
- **Engine A & B** (`gpu2d/engine_a.rs`, `gpu2d/engine_b.rs`) — share a generic engine struct parameterized by which I/O page and which OAM/palette/VRAM banks it can see. Engine B has reduced features (no 3D source, no extended palette banks F/G, smaller BG limits).
- **BG modes**: text (mode 0-1 BGs), affine (mode 0-2 BGs), extended affine modes (256-color bitmap / direct-color bitmap / 16-color tile w/ 8-bit map), large-screen 512×1024 affine.
- **OBJ**: 128 sprites × 8 bytes, plus optional ext-OAM giving 1024 sprites; 1D mapping with configurable boundary (32/64/128/256 B). 3D-source OBJ (mode = 3) — Engine A only — overlays the 3D framebuffer.
- **Compositing** — same priority-and-layer model as GBA, plus a "3D layer" sourced from Engine A's 3D unit when DISPCNT bit 3 is set.
- **Display capture unit** (`gpu2d/capture.rs`) — Engine A can snapshot its output (or 3D output) into a VRAM bank. Used by games that compose using both engines plus a captured frame.

**Tests planned:**
- VRAMCNT routing: write to Bank A, configure as Engine B BG, read via Engine B's BG window.
- Render a test ROM with one BG layer of solid red on Engine A, sample framebuffer.
- Render a homebrew that uses extended affine + OBJ to confirm priority ordering.

### Phase 4: DMA + Timers + IPC + Input

**Goal**: Make the system interactive and IRQ-driven. Both CPUs' DMA channels work for all NDS-specific start modes; both CPUs' timers tick correctly with cascade; IPC FIFO and Sync registers ferry messages between CPUs; touch + buttons + lid produce interrupts.

**Plan:**
- **DMA9** (`dma/arm9.rs`) — 4 channels with NDS start modes: Immediate, VBlank, HBlank (per scanline 0..191), Display (during HDraw), Slot-1 cart, GXFIFO half-empty, MainMemory display FIFO.
- **DMA7** (`dma/arm7.rs`) — 4 channels with ARM7 start modes: Immediate, VBlank, Slot-1 cart, AUXSPI cart, sound DMA (timer-triggered, FIFO-style for sound channels 1-3, 5-7, 9-11, 13-15? — actually NDS uses 4 sound DMA channels, distinct from sound channels), wireless (stubbed).
- **Timers9 / Timers7** — port from GBA, two instances. Cascade and prescaler logic identical; only the IRQ source list differs.
- **IPC FIFO** — 16-deep × 32-bit FIFO each direction, with `IPCFIFOSEND`/`IPCFIFORECV` registers and `IPCFIFOCNT` flags (empty/full, error/clear, IRQ enables). Reads from an empty FIFO trigger error flag, not stall (per spec).
- **IPC Sync** — `IPCSYNC` 4-bit-each-direction register with an IRQ on receive change.
- **Keypad / EXTKEYIN** — `KEYINPUT` (ARM9 + ARM7) for the 10 GBA-style buttons; `EXTKEYIN` (ARM7 only) for X/Y/Pen/Lid/Debug. Pen down comes from the touch driver in the next phase.

**Tests planned:**
- Send 16 words ARM9→ARM7 via IPC FIFO, verify ARM7 reads them in order.
- HBlank DMA on ARM9 line 0..191 only (not VBlank).
- GXFIFO DMA: write a packed command list to RAM, configure DMA, observe GXFIFO accepting words.

### Phase 5: SPI Bus + AUXSPI Backup + Firmware Boot Hooks

**Goal**: Touch input works; firmware settings are readable; cart backup persists.

**Plan:**
- **SPI controller** (`spi/mod.rs`) — `SPICNT`/`SPIDATA` arbitrate over three devices selected by `SPICNT[8:9]`: Power (PMIC), Firmware (256 KB SPI flash), Touchscreen (12-bit ADC).
- **Firmware** (`spi/firmware.rs`) — 256 KB image. We implement READ (0x03), READ_STATUS (0x05), WRITE_ENABLE (0x06), PAGE_WRITE (0x0A), SECTOR_ERASE (0xD8). User settings block (with checksum) at offset `0x3FE00` (image size minus 0x200) — our direct-boot path can synthesize a default user settings block if no firmware dump is provided so games that read nickname/birthday don't crash.
- **TSC** (`spi/tsc.rs`) — emulates the ADS7843 protocol: control byte selects which axis to digitize; subsequent transfer returns 12-bit X/Y/Z. Touch coordinates from SDL2 mouse get translated to ADC values using the calibration data from firmware.
- **PMIC** (`spi/pmic.rs`) — minimal: register 0 (control: backlights, sound enable, power), register 4 (battery status). Returns sane defaults; writes are mostly observable via traces.
- **AUXSPI** (`cart/auxspi.rs`) — runs over slot-1's auxiliary SPI. Backup type detection per `gamecode` (ROM database) or via header byte; supports EEPROM 0.5K/8K/64K, FRAM 32K, FLASH 256K/512K/1M, NAND (rare).
- **DMA carry-over from Phase 4**: wire the `Slot1` (and `Slot2` if we ever turn it on) DMA start modes — Phase 4 implemented the start-mode decode but no trigger fires them yet. When the slot-1 controller transitions "data word ready" (after a cart command transfer), the cart code must call `Nds::run_dmas_for_timing9(DmaTiming::Slot1)` (and `_for_timing7` if `EXMEMCNT` routes the slot to ARM7).

**Tests planned:**
- Touch a pixel on the bottom screen via SDL2 mouse; confirm TSC returns the expected ADC value.
- Read firmware user settings via SPI; verify nickname matches the configured default.
- Boot a small homebrew that issues an AUXSPI EEPROM write, verify on save export.
- Slot-1 read transfer: configure DMA channel for `Slot1` timing, issue a cart read, verify the DMA fires and copies the cart response into Main RAM.

### Phase 6: 3D Geometry Pipeline

**Goal**: Vertex pipeline through the geometry stage. Matrix stacks, vertex transformations, lighting, polygon assembly, clipping, and viewport transform. Output is a list of clipped, lit polygons in screen space, ready for the rasterizer.

**Plan:**
- **GXFIFO** (`gpu3d/fifo.rs`) — 256-entry × 32-bit command FIFO. Commands written via `GXFIFO` (0x04000400) packed format or via direct port `GXCMD_xx` (0x04000440..). Drain FIFO into a command queue; each command consumes 0..32 parameter words. GXSTAT reports FIFO half-empty / empty / full + busy. Trigger DMA9 GXFIFO start mode when FIFO is half-empty.
- **Matrix stacks** (`gpu3d/matrix.rs`) — projection (1-deep), position (32-deep), position+vector pair (32-deep, both updated together when in MTX_MODE 2), texture (1-deep). 4×4 fixed-point 1.19.12 (we'll use `i32` per element).
- **Vertex pipeline** (`gpu3d/vertex.rs`) — `BEGIN_VTXS` selects primitive type (triangle/quad/triangle-strip/quad-strip); `VTX_16`/`VTX_10`/`VTX_XY`/`VTX_XZ`/`VTX_YZ`/`VTX_DIFF` feeds vertex coords. Each vertex gets transformed by clip = projection × position × vertex; clip-space → light → texture-coord transform → output.
- **Lighting** (`gpu3d/lighting.rs`) — 4 directional lights, diffuse + ambient + specular (Phong via reflective table), per-vertex.
- **Polygon assembly** — combine vertices into triangles/quads; track polygon attribute (`POLYGON_ATTR`: light enable, fog, alpha, ID, mode, cullface).
- **Clipping** (`gpu3d/clip.rs`) — 6-plane Sutherland-Hodgman in clip space against ±W. Reject fully outside; pass through fully inside; clip if straddling.
- **Viewport** — apply VIEWPORT command, divide by W (perspective), output `screen_x`, `screen_y`, `z`, `w`, `s`, `t`, `r`, `g`, `b`, `a` per vertex.
- **POLYGON_RAM swap** — `SWAP_BUFFERS` (cmd 0x50) flushes geometry to the rasterizer-side buffer (up to 2048 polygons, 6144 vertices) for the next frame.
- **DMA carry-over from Phase 4**: wire the `GxFifo` DMA start mode. Phase 4's `DmaController` already decodes ARM9 timing bits 27..29 = 0b111 as `DmaTiming::GxFifo`, but no event source fires it. When the GXFIFO drains below 128 entries (half-full), the geometry pipeline must call `Nds::run_dmas_for_timing9(DmaTiming::GxFifo)` so DMA channels armed for that mode push more command words.

**Tests planned:**
- Push a single triangle through the pipeline, verify output matches GBATEK math example.
- Verify projection matrix stack push/pop count.
- Clip a triangle that crosses the near plane; verify two output triangles.
- GXFIFO DMA: configure DMA9 channel for `GxFifo` timing, write a packed command list to RAM, observe DMA pushing into the GXFIFO when the queue drops below 128 entries.

### Phase 7: 3D Rasterizer + Display Output

**Goal**: Rasterize the polygon buffer into a 256×192 framebuffer that Engine A can composite. Depth buffer (Z or W), per-pixel alpha, edge marking, fog, anti-aliasing, toon/highlight shading, capture unit.

**Plan:**
- **Rasterizer** (`gpu3d/raster.rs`) — span-based, scanline iteration of each polygon (sorted: opaque first by Y, translucent after). For each span, interpolate Z/W, color, U/V, fog factor.
- **Texture unit** (`gpu3d/texture.rs`) — 8 formats: A3I5, 2bpp, 4bpp, 8bpp, 4×4-block compressed (with palette in slot 1), A5I3, direct color (16-bit), and "no texture" (color from VTX_COLOR + lighting). Texture image data lives in VRAM banks routed to "texture image"; texture palette in VRAM banks routed to "texture palette".
- **Depth + alpha** — `DISP3DCNT` controls Z-buffer vs W-buffer, alpha test threshold, blending mode, edge marking, fog enable, anti-alias enable. Translucent polygons read+write alpha but only write Z if `POLYGON_ATTR.depth_update_for_translucent`.
- **Edge marking** — pixels at polygon-ID boundaries get tinted with `EDGE_COLOR` table (8 entries).
- **Fog** — table-driven (32-entry) interpolation between fog color and pixel color based on depth/Z threshold.
- **Anti-alias** — coverage-based supersampling on triangle edges (we implement a simple 8x edge AA mask).
- **Toon/Highlight** — DISP3DCNT bit 1: replace red channel with TOON_TABLE entry (toon shading) or add it (highlight shading).
- **Display capture** — wire the 3D framebuffer into Engine A's capture path so games that capture-then-blend work.

**Tests planned:**
- Solid-color triangle rasterizes the right pixel set.
- Z-buffer correctly rejects a far triangle behind a near one.
- Fog table darkens distant pixels.
- A first commercial 3D ROM (e.g. a homebrew like `osmash` or `nds-bootstrap`'s test patterns) boots to a recognizable 3D scene.

### Phase 8: Audio + Save States

**Goal**: 16-channel mixed audio output; AUXSPI save export/import; full-state save/load.

**Plan:**
- **Audio engine** (`audio/mod.rs`) — 16 channels each with: format (PCM8/PCM16/IMA-ADPCM/PSG), source address, length, loop start/length, volume (0-127, with shift), pan (0-127), repeat mode (manual/loop/one-shot).
- **PSG channels** — channels 8-13 can be square-wave PSG with duty (8 duty values), channels 14-15 are noise (LFSR).
- **IMA-ADPCM** — 4-bit ADPCM with 1-word header per block (16-bit predictor + 16-bit step index); decoder advances per sample.
- **Sound DMA** — when `CHCNT.format == ADPCM` and the loop start/total length are configured, the channel cycles through the source address; we don't actually need DMA for sample fetch (channels read directly from main RAM/VRAM via the bus). The "sound DMA" label refers to the 4 ARM7 DMA start modes triggered by sound channels.
- **Capture units** — 2 capture units that record either the mixer output or a specific channel into a buffer in main RAM. Used for echo/reverb.
- **Save states** — `Nds::save_state()` / `load_state()` via bincode + zstd, mirrors GBA approach.
- **AUXSPI export/import** — `Nds::export_save()` returns the raw backup bytes; `import_save()` accepts a `.sav` file.

**Tests planned:**
- Play a sine wave through one PCM16 channel, verify mixer output.
- Save state mid-frame, load, run another frame, compare framebuffer.
- ADPCM round-trip: encode-then-decode a known signal.

### Phase 9: Accuracy Polish + Debugger Utilities

**Goal**: Compatibility with a target set of commercial titles (initial goal: 5 titles boot to title screen; stretch goal: 20+ playable to first save).

**Plan (tracked one bug at a time, each with a `debug/<date>_<short-name>.md`):**
- ARM9 cache emulation — most games don't depend on it but a few prefetch quirks bite us. We'll add a write-through cache simulator if needed.
- Memory wait states — `EXMEMCNT` GBA-slot timing, Main RAM access penalty (8/9-cycle ARM9, 4-cycle ARM7).
- 3D edge cases — degenerate polygons (zero-area, all-clip), W-buffer mode, depth precision in projection matrix corner cases.
- Slot-1 cart timing — RTC is read via the cart's KEY2 stream too, not the SPI bus.
- Boot logo video — DSi-specific; we skip.
- Audio channel timing edge cases — start/stop while DMA in flight.

## Project Structure (planned)

```
nds/
├── Cargo.toml                    # Workspace root
├── PLAN.md                       # This file
├── ARCHITECTURE.md               # Technical architecture deep-dive
├── README.md                     # Quick start / build / run
├── .gitignore
│
├── nds-core/                     # Library crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                # Nds top-level, run_frame()
│       ├── cpu/
│       │   ├── mod.rs            # Cpu struct (parameterized for ARM9/ARM7)
│       │   ├── arm.rs            # ARM decoder (covers ARMv4T + ARMv5TE)
│       │   ├── thumb.rs          # THUMB decoder (covers ARMv4T + v5T BLX)
│       │   ├── alu.rs            # Barrel shifter + ALU
│       │   ├── cp15.rs           # CP15 system control coprocessor (ARM9)
│       │   └── disasm.rs         # Disassembler (Phase 9, on demand)
│       ├── bios/
│       │   ├── arm9.rs           # ARM9 BIOS HLE
│       │   └── arm7.rs           # ARM7 BIOS HLE
│       ├── bus/
│       │   ├── arm9.rs           # ARM9-side bus
│       │   ├── arm7.rs           # ARM7-side bus
│       │   ├── shared.rs         # Main RAM, shared WRAM, palette/OAM/VRAM, IPC
│       │   ├── io_arm9.rs        # ARM9 I/O register dispatch
│       │   └── io_arm7.rs        # ARM7 I/O register dispatch
│       ├── vram.rs               # VRAMCNT_A..I routing
│       ├── gpu2d/
│       │   ├── mod.rs            # Engine struct, compositing, capture
│       │   ├── engine_a.rs       # Engine A wiring (full feature set)
│       │   ├── engine_b.rs       # Engine B wiring (subset)
│       │   ├── bg.rs             # All BG modes including extended affine
│       │   ├── obj.rs            # OBJ (incl. ext-OAM, 3D-source mode)
│       │   ├── window.rs         # Window 0/1/OBJWIN (same shape as GBA)
│       │   ├── effects.rs        # Blend / brightness
│       │   └── capture.rs        # Display capture unit
│       ├── gpu3d/
│       │   ├── mod.rs            # 3D engine top-level
│       │   ├── fifo.rs           # GXFIFO command FIFO
│       │   ├── matrix.rs         # Matrix stacks + math
│       │   ├── vertex.rs         # Vertex pipeline
│       │   ├── lighting.rs       # Per-vertex lighting
│       │   ├── clip.rs           # Sutherland-Hodgman 6-plane clip
│       │   ├── raster.rs         # Span-based rasterizer
│       │   └── texture.rs        # 8 texture formats + palette
│       ├── audio/
│       │   ├── mod.rs            # 16-channel mixer
│       │   ├── channel.rs        # Per-channel state
│       │   ├── adpcm.rs          # IMA-ADPCM decode
│       │   └── capture.rs        # Capture units
│       ├── dma/
│       │   ├── arm9.rs           # ARM9 DMA + GXFIFO/HDMA/Display modes
│       │   └── arm7.rs           # ARM7 DMA + Sound DMA
│       ├── timer.rs              # Per-CPU timers
│       ├── interrupt.rs          # Per-CPU IE/IF/IME
│       ├── ipc.rs                # IPC FIFO + Sync
│       ├── keypad.rs             # KEYINPUT + EXTKEYIN
│       ├── spi/
│       │   ├── mod.rs            # SPI bus arbiter
│       │   ├── firmware.rs       # 256 KB firmware flash
│       │   ├── tsc.rs            # Touchscreen ADC
│       │   └── pmic.rs           # Power management chip
│       ├── cart/
│       │   ├── mod.rs            # Cart slot-1 controller
│       │   ├── header.rs         # NDS header parse + CRC
│       │   ├── direct_boot.rs    # BIOS-equivalent RAM loader
│       │   ├── auxspi.rs         # AUXSPI backup (EEPROM/FRAM/FLASH)
│       │   └── key.rs            # KEY1/KEY2 (Phase 9 if needed for encrypted carts)
│       ├── rtc.rs                # S-3511 RTC (date/time)
│       └── scheduler.rs          # Min-heap event scheduler
│
├── nds-frontend/                 # Binary crate
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs               # Entry, arg parse, main loop
│       ├── video.rs              # Dual-screen window, BGR555→RGB24
│       ├── audio.rs              # SDL2 audio callback + ring buffer
│       └── input.rs              # Keyboard + mouse → buttons + touch
│
├── debug/                        # Per-bug incident notes (added as we hit bugs)
└── test-roms/                    # .gitignored, user-supplied test ROMs
```

## Testing Strategy

- **Unit tests** per subsystem, same model as GBA: barrel shifter, every ARM/THUMB/v5TE instruction, every BIOS SWI, BG/OBJ rendering, window, blending, VRAM routing, IPC FIFO ordering, GXFIFO command queue, matrix stack, clipping math, ADPCM round-trip.
- **Integration tests** at the `Nds` top-level: hand-assembled ARM9/ARM7 binaries that exercise a specific path (e.g. ARM9 writes a value to Main RAM, ARM7 reads it).
- **Test ROMs**: rockwrestler, bigredpimp's NDS test, jsmolka armwrestler-ds (has both ARM7 and ARM9 modes), gbatek's 3D demos.
- **Screenshot comparison** against melonDS / no$gba reference frames.
- **Trace comparison** against melonDS instruction logs for bring-up.

## Dependencies

```toml
# nds-core
serde = { version = "1", features = ["derive"] }
bincode = "1"
log = "0.4"

# nds-frontend
nds-core = { path = "../nds-core" }
sdl2 = { version = "0.37", features = ["bundled", "static-link"] }
clap = { version = "4", features = ["derive"] }
env_logger = "0.11"
zstd = "0.13"
```

## Reference

- GBATEK: https://problemkaputt.de/gbatek.htm (primary spec)
- melonDS source (cross-check on ambiguous behavior)
- jsmolka armwrestler-ds (CPU validation)
- TONC (carries over for many 2D concepts since Engine A is GBA-PPU-like)
- The sibling GBA project at `../gba` (architectural reference)

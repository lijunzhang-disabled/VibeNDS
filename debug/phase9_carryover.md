# Phase 9 carry-over checklist

Items deliberately deferred during Phases 1-7. None block typical homebrew or 2D-only commercial games from running; most matter only when we start exercising specific game behaviors that the simpler implementations get wrong.

This file is the canonical "what's left" list. Each entry has: where it was deferred, what the current behavior is, what real hardware does, and a rough trigger ("when does a game break because of this?").

---

## 3D engine — Phase 7 deferrals

### 3D-1. Format-5 (4×4 block-compressed) texture decoder
- **Status**: Implemented with focused coverage.
- **Current**: `gpu3d/raster/texture.rs::sample_block_compressed` decodes block indices, mode bits, palette offsets, transparent mode-0/1 index 3, mode-1 average color, mode-2 explicit colors, and mode-3 weighted colors. Slot-2 data uses the upper half of the slot-1 parameter table, and `PLTT_BASE` offsets the palette lookup for compressed textures.
- **Hardware**: 4×4 blocks of pixels share a single block header in texture-image VRAM + a per-block palette in texture-palette VRAM. Decodes by combining 2 base colors with 2 interpolated colors via a 2-bit-per-texel index.
- **Remaining risk**: Broader image-diff tests against known compressed-texture ROMs would still be useful, but this is no longer a stubbed/missing feature.

### 3D-2. Anti-aliasing
- **Status**: Partially implemented; still approximate.
- **Current**: `DISP3DCNT` bit 4 softens opaque silhouette pixels using polygon-ID/depth neighborhood checks. When AA is enabled, the rasterizer clips each covered pixel square against the triangle and records a coverage value from the clipped area, plus an edge-direction bitmask for coverage-limited pixels. The AA pass uses those hints instead of a fixed 50% blend when available, and tries hinted neighbors before fallback scan order. Visible 3D edges blend toward the exposed neighbor pixel; opaque rear-plane edges blend toward rear color; transparent rear-plane exposure is alpha-only so Engine A can composite BG0-from-3D over the actual 2D layer underneath. Existing interactions remain covered: translucent pixels are skipped, zero-dot polygons follow the edge-marking quirk, same-polygon interior pixels remain opaque, and rear bitmap clears provide the opaque fallback blend color when their alpha bit is set.
- **Hardware**: coverage-based on triangle edges — fractional pixel coverage stored per-pixel during rasterization, edge pixels blended with their cross-edge neighbor by the coverage value.
- **Validation so far**: HeartGold title-loop sweep through frame 5400 on
  2026-06-06 produced coherent sampled frames at 540-frame intervals, including
  the title frames around 4320 and 5400, with no recurrence of the earlier
  random-polygon flashing and no large artificial screen gap.
- **Remaining risk**: Needs image-level confirmation against hardware/reference
  captures for complex AA edge intersections; sampled commercial-game frames are
  useful smoke tests, but they are not pixel-level conformance.

### 3D-3. Toon / highlight via `POLYGON_ATTR.mode = 2`
- **Status**: Implemented with focused coverage.
- **Current**: `combine_toon_highlight` uses the toon table for mode-2 polygons, and `DISP3DCNT` bit 1 selects highlight addition versus toon replacement.
- **Hardware**: red channel of the per-vertex color (after lighting + texture combine) gets remapped through the 32-entry `TOON_TABLE`. `DISP3DCNT` bit 1 selects toon (replace) vs highlight (add).
- **Remaining risk**: Needs broader visual ROM coverage for cel-shaded scenes, especially combined with texture alpha and fog.

### 3D-4. Shadow polygon mode (`POLYGON_ATTR.mode = 3`)
- **Status**: Implemented with focused coverage.
- **Current**: The rasterizer tracks a shadow stencil. Polygon ID 0 writes the mask, visible shadow polygons draw only where the mask bit is set, same-ID rejection is preserved, the consumed mask bit is cleared, and polygon alpha controls shadow intensity.
- **Hardware**: two-pass: shadow-mask pass writes 1s to a per-pixel mask; shadow-volume pass darkens pixels where the mask is 1 and the destination polygon ID differs from the visible shadow polygon ID.
- **Remaining risk**: Needs image-level ROM coverage for complex shadow volumes and overlaps.

### 3D-5. W-buffer mode
- **Status**: Implemented with focused coverage.
- **Current**: `SWAP_BUFFERS` bit 1 selects W-buffering for depth tests, and fog lookup follows the active depth mode. The rasterizer has draw-path coverage for W-depth ordering, inclusive equal-depth tolerance, and rejection just outside that tolerance.
- **Hardware**: `SWAP_BUFFERS` bit 1 selects W (= raw w) vs Z (= z/w). Different precision distribution: W is uniform across the frustum, Z is more precise near the camera.
- **Remaining risk**: More image-level ROM coverage would help catch subtle W overflow/clamping issues in real scenes.

### 3D-6. Display capture from 3D framebuffer
- **Status**: Implemented for the covered Engine A paths.
- **Current**: `DISPCAPCNT` can arm capture on the next visible line 0, read Engine A/3D source A, consume main-memory FIFO source B, blend, and write to the selected VRAM block with wrapping behavior. Capture output is packed by selected capture width, including compact 128-pixel row stride for 128×128 captures and 256-pixel row stride plus height cutoff for 256×64 and 256×128 captures.
- **Hardware**: Engine A can capture its output (or just the 3D framebuffer, or a blend) into a VRAM bank for use as a texture next frame. Enables motion blur, screen distortion, picture-in-picture effects.
- **Remaining risk**: Needs game-level coverage for feedback effects and all capture source/size combinations.

### 3D-7. Box / position / vector test commands
- **Status**: Implemented with focused coverage.
- **Current**: `BOX_TEST`, `POS_TEST`, and `VEC_TEST` produce visible/result-register state, clear the test-busy state, and are covered through the GX command path with live matrix state. `BOX_TEST` follows the hardware face-clipping quirk where a box enclosing the whole view volume reports not visible because no box face intersects the view volume.
- **Hardware**: hardware-side frustum tests that return results via `GXSTAT`. Used by games for view-frustum culling on the ARM9 side.
- **Remaining risk**: More ROM-level visibility/culling coverage would help, but this is no longer a no-result stub.

---

## CPU / bus accuracy

### CPU-1. ARM9 cache simulation
- **Current**: cache control ops via CP15 c7 (clean / invalidate / drain) are NOPs. Every access goes to the bus.
- **Hardware**: 8 KB I-cache, 4 KB D-cache, both 4-way set associative, 32-byte lines. Most games tolerate a write-through-cache-with-immediate-invalidate model (which is effectively transparent), but a few rely on explicit cache flushes for DMA coherency.
- **Trigger**: games that DMA to RAM and then read from the same lines might see stale "cached" data. We currently get this right by accident (no cache to be stale), but a future "fast path" cache might re-introduce the bug.

### CPU-2. Memory wait states
- **Current**: every memory access is 1 cycle.
- **Hardware**: Main RAM is 8/9 cycles on ARM9, 4 cycles on ARM7 (per `EXMEMCNT`). ROM has its own wait timings. Net: we're 10-30% faster than real hardware.
- **Trigger**: cycle-tight games (precision platformers, anything polling I/O in inner loops). Most games don't notice.

### CPU-3. Open-bus accuracy
- **Current**: unmapped reads return 0.
- **Hardware**: unmapped reads return the last value latched on the bus (CPU fetch, last DMA word, etc.). Edge case for games that probe memory boundaries.
- **Trigger**: certain copy-protection schemes; jsmolka-style accuracy test ROMs.

### CPU-4. Misaligned access quirks
- **Current**: `LDR` / `LDRH` rotated-result quirks implemented for ARM7; force-aligned for ARM9. Need a focused review against the ARM ARM spec.
- **Hardware**: ARM7TDMI has rotated-result behavior for misaligned word loads (`addr & 3` rotates result right). Halfword: similar rotation. ARM9 force-aligns. Already-fixed for the common cases.
- **Trigger**: edge-case usage of misaligned addresses, mostly seen in jsmolka tests.

---

## Cart / boot

### Cart-1. KEY1 / KEY2 encrypted cart support
- **Current**: deferred from Phase 5. Direct boot only — assumes the ROM secure area is already decrypted.
- **Hardware**: cart's first 0x4000 bytes are encrypted with KEY1 (Blowfish-style, key derived from gamecode + BIOS P-array). The slot-1 command stream is encrypted with KEY2 (additive stream cipher).
- **Trigger**: original (non-redumped) ROMs won't boot. Most ROM-set ROMs are pre-decrypted so this only matters for header-CRC validation or true-original-cart usage.

### Cart-2. Slot-1 RTC reads via the cart bus
- **Current**: RTC is exposed only via the SPI bus (Phase 5).
- **Hardware**: some games read RTC through the cart's KEY2 stream too — not the SPI path.
- **Trigger**: Pokémon, Animal Crossing, anything that does time-based events; could read wrong time of day depending on the access path.

### Cart-3. Slot-1 ROM transfer machine
- **Current**: AUXSPI backup path works, and the minimal unencrypted Slot-1
  command path handles header reads, chip ID reads, normal `B7` ROM reads,
  transfer-ready status, Slot-1 data IRQ, and Slot-1 DMA from both ARM9 and
  ARM7. The direct-boot path also mirrors the loaded ROM bytes into the shared
  Slot-1 backing store for runtime NitroFS/card reads.
- **Hardware**: ARM9 writes 8-byte commands to `ROMCTRL` + `ROMCMD`; cart returns up to 0x4000 bytes via `ROMDATAIN`. Most games never touch this after direct boot reads the ARM9/ARM7 binaries, but games that *load assets from cart at runtime* (e.g. *Pokémon* level data, voice clips) need this.
- **Remaining risk**: The encrypted KEY1/KEY2 protocol, detailed transfer
  timing, card ownership arbitration, and less common cart commands are still
  approximate. A game that depends on true encrypted command sequencing or
  cycle-level card timing can still fail after the simple direct-boot/runtime
  read path succeeds.

### Cart-4. Cart backup type ROM database
- **Current**: `--save-type` CLI flag forces the type; default is EEPROM 64K via header `device_capacity`. Header byte isn't reliable.
- **Hardware**: gamecode → backup-type table maintained by the homebrew community.
- **Trigger**: any commercial game whose default doesn't match its real backup chip will load/save corrupt data until `--save-type` is set right.

---

## Audio — Phase 8 part 3 / Phase 9

### Audio-1. Sound DMA `Special` start mode wiring
- **Current**: Phase 8 implemented all 16 channels reading samples directly from main RAM via the bus_read8 closure. ARM7 DMA channels 1-3 armed for `DmaTiming::Special` don't fire from sound channels yet.
- **Hardware**: when a sound channel's loop point is hit (or first-fill on restart), a configured ARM7 DMA channel can be triggered to refill the buffer.
- **Trigger**: streaming audio (background music with multi-buffer ping-pong) doesn't get refilled, so it cuts off after one buffer.
- **Note**: per the cross-trigger lesson from `../gba/debug/2026-05-05_srtog-fifo-b-cross-trigger.md`, gate by per-channel demand, not by class.

### Audio-2. VRAM-resident audio samples
- **Current**: `mixer::tick` reads sample data only from main RAM (the `bus_read8` closure masks to `addr & 0x3F_FFFF`).
- **Hardware**: sample data can live in VRAM (typically bank C/D when ARM7-mapped). Games doing streaming audio often DMA into VRAM and play from there.
- **Trigger**: VRAM-resident audio plays silence.

### Audio-3. ADPCM block-loop edge cases
- The ADPCM step-index can wrap if a sample loops over a block boundary mid-step. The current loop-point predictor snapshot helps but doesn't cover every edge case.

### Audio-4. Capture units
- Two capture units that record either the mixer output or a single channel into a buffer. Used for echo/reverb effects.

---

## Diagnostics / dev experience

### Diag-1. `OnceLock<bool>` cache pattern for env-gated diagnostics
- See [`debug/README.md`](README.md). When we add per-instruction or per-memory-access diagnostic switches, cache the env-var lookup in `OnceLock<bool>` — never call `std::env::var` directly in hot paths.

### Diag-2. Instruction trace ring buffer
- Port the GBA's `INSTR_TRACE_RING` ring buffer for "freeze on first invalid PC, dump last 256 instructions." Useful for the first commercial game that escapes the CPU.

### Diag-3. Multi-game compatibility sweep
- Run a curated set of 10-20 commercial games. Each first-boot failure becomes a Phase 9 entry. The GBA project's `followups.md` lists this as its top-of-funnel for finding accuracy bugs.

---

## How to use this file

When you start working on a Phase 9 item:
1. Pull the entry into a new `debug/YYYY-MM-DD_<slug>.md` investigation log.
2. Use the GBA project's template (symptom → investigation → root cause → fix → regression test → verification).
3. Delete the entry from this file once the fix lands.

When something new turns up during Phase 8 or runtime testing that doesn't warrant its own debug doc yet, add a one-paragraph entry here.

# Phase 9 carry-over checklist

Items deliberately deferred during Phases 1-7. None block typical homebrew or 2D-only commercial games from running; most matter only when we start exercising specific game behaviors that the simpler implementations get wrong.

This file is the canonical "what's left" list. Each entry has: where it was deferred, what the current behavior is, what real hardware does, and a rough trigger ("when does a game break because of this?").

---

## 3D engine — Phase 7 deferrals

### 3D-1. Format-5 (4×4 block-compressed) texture decoder
- **Current**: returns transparent for every texel.
- **Hardware**: 4×4 blocks of pixels share a single block header in texture-image VRAM + a per-block palette in texture-palette VRAM. Decodes by combining 2 base colors with 2 interpolated colors via a 2-bit-per-texel index.
- **Trigger**: many commercial games use this format for opaque textures (it's the most space-efficient option for full-color textures). Visible as missing/transparent textures.
- **Spec**: `gpu3d/raster/texture.rs::sample_block_compressed` has the stub.

### 3D-2. Anti-aliasing
- **Current**: `DISP3DCNT` bit 4 is loadable but has no effect.
- **Hardware**: coverage-based on triangle edges — fractional pixel coverage stored per-pixel during rasterization, edge pixels blended with their cross-edge neighbor by the coverage value.
- **Trigger**: jagged edges on polygon silhouettes. Cosmetic; never breaks gameplay.

### 3D-3. Toon / highlight via `POLYGON_ATTR.mode = 2`
- **Current**: `combine_with_vertex` treats mode 2 the same as mode 0 (modulate). The toon table is loaded into `rasterizer.toon_table` but never consulted.
- **Hardware**: red channel of the per-vertex color (after lighting + texture combine) gets remapped through the 32-entry `TOON_TABLE`. `DISP3DCNT` bit 1 selects toon (replace) vs highlight (add).
- **Trigger**: cel-shaded games (e.g. *Trauma Center*, *Mario vs Donkey Kong: March of the Minis*) lose their characteristic banded shading look.

### 3D-4. Shadow polygon mode (`POLYGON_ATTR.mode = 3`)
- **Current**: stubbed — returns the vertex color unmodified in `combine_with_vertex`.
- **Hardware**: two-pass: shadow-mask pass writes 1s to a per-pixel mask; shadow-volume pass darkens pixels where the mask is 1 AND polygon-ID matches.
- **Trigger**: missing shadows under 3D characters in most action games.

### 3D-5. W-buffer mode
- **Current**: always Z-buffer (depth = z/w from NDC).
- **Hardware**: `DISP3DCNT.depth_buffer_mode` (bit not currently checked) selects W (= raw w) vs Z (= z/w). Different precision distribution — W is uniform across the frustum, Z is more precise near the camera.
- **Trigger**: Z-fighting on close coplanar polygons in games that explicitly select W-buffer for those scenes.

### 3D-6. Display capture from 3D framebuffer
- **Current**: `DISPCAPCNT` is not wired (Phase 3 stub).
- **Hardware**: Engine A can capture its output (or just the 3D framebuffer, or a blend) into a VRAM bank for use as a texture next frame. Enables motion blur, screen distortion, picture-in-picture effects.
- **Trigger**: any game that uses these effects loses them silently.

### 3D-7. Box / position / vector test commands
- **Current**: `BOX_TEST`, `POS_TEST`, `VEC_TEST` are decoded and consumed by the GXFIFO but produce no result.
- **Hardware**: hardware-side frustum tests that return results via `GXSTAT`. Used by games for view-frustum culling on the ARM9 side.
- **Trigger**: games that rely on these tests for early culling will draw everything (slow) but should still render correctly. Performance issue, not a correctness one — but could land here as a regression test if a game becomes unplayably slow.

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
- **Current**: AUXSPI backup path works; the actual cart ROM-read command machine is stubbed.
- **Hardware**: ARM9 writes 8-byte commands to `ROMCTRL` + `ROMCMD`; cart returns up to 0x4000 bytes via `ROMDATAIN`. Most games never touch this after direct boot reads the ARM9/ARM7 binaries, but games that *load assets from cart at runtime* (e.g. *Pokémon* level data, voice clips) need this.
- **Trigger**: a game that direct-boots fine but freezes at the first "loading next area" prompt.
- **Note**: when this lands, fire `Nds::run_dmas_for_timing9/7(DmaTiming::Slot1)` on each "data word ready" transition — Phase 4 DMA carry-over.

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

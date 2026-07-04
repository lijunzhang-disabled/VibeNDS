# Emulator slowdown — debug instrumentation ran unconditionally on hot paths

Date: 2026-07-04
Status: **Fixed** (48.9 → ~78 fps in the HGSS overworld; above the DS's 60)

## Symptom

Game feels laggy/slow in the SDL frontend. Headless benchmark (300 frames of
the HGSS bedroom scene via the harness): **48.9 fps** — below real-time.

## Causes

The debugging instrumentation accumulated over the HGSS conformance work ran
unconditionally in normal play:

1. **Uncached `std::env::var_os` checks on hot paths.** Rust env lookups
   take a global lock and scan the environment. 27 call sites checked
   trace variables per event, the worst being:
   - `NDS_FORMAT3_PLTT_DIV8` — **per texel sample** in the 3D rasterizer
   - `NDS_TRACE_BUS9_MAIN_READ_VALUE` — per ARM9 main-RAM `read32`
   - `NDS_TRACE_DMA9_VALUE` — per DMA word
   - `NDS_TRACE_VRAM_BANK_RANGE` (with a full range re-parse) — per VRAM
     byte written
   - `NDS_TRACE_AUXSPI` — per backup SPI byte

2. **3D debug capture allocated per GX command / per polygon:**
   - `push_debug_op`: `format!("{:?}", cmd)` + `params.clone()` for every
     geometry command (thousands per frame)
   - `record_debug_screen_polygon`: several Vec collects + a clone of the
     48-deep op history **per polygon**, into a 2048-entry ring using
     `Vec::remove(0)` (O(n) shift once full)
   - `record_rejected_polygon`: similar, with a `String` reason
   - `TextureSnapshot::capture` per polygon per frame — a whole-texture
     copy routed byte-by-byte through the 9-bank VRAM scan. The normal
     render path samples **live VRAM** and never reads these snapshots;
     they only feed the harness debug JSON and VRAM-less re-renders, so
     this cost was pure waste in normal play.

3. Minor: `Timers::tick` (called ~2M×/s per CPU) did div/mod per enabled
   timer — prescalers are powers of two, now shift/mask, plus a one-branch
   early-out when no timer runs. `trace_arm9_exec` computed the adjusted PC
   per instruction before checking whether tracing was even configured.

## Fix

- All env toggles cached in `OnceLock` statics (read once per process; the
  existing range-parse hooks already did this — the pattern is now applied
  everywhere).
- All 3D debug capture gated behind **`NDS_DEBUG_3D=1`** (cached), off by
  default: op history, screen/rejected polygon records, texture snapshots.
- Timer prescaler shift/mask + idle early-out; exec-trace early-out.

**Tooling note:** debug scripts that read `screen_polygon_debug`,
`rejected_polygons`, `texture_snapshot`, or `recent_ops` from the harness's
`get_3d_debug` JSON must now set `NDS_DEBUG_3D=1` in the harness env.
Env toggles are also now read once — set them at process spawn, not
mid-session.

## Results

Benchmark (harness, 300 frames, HGSS bedroom, M-series host):

- before: 48.9 fps
- after: 77.7–86.0 fps across runs (~1.6×), above the DS's 60 fps

673/673 tests pass; bedroom scene renders identically (normal rendering
never consumed the gated data).

## Remaining hot spots (future work, structural)

Post-fix profile (`sample`, 5 s of overworld):

- `VramRouter::read_engine_{a,b}_bg` — per-byte linear scan over all 9
  banks for every 2D BG fetch (~10 %). A per-16KB-page lookup table
  rebuilt on VRAMCNT writes would remove it.
- `GxOp::params: Vec<u32>` per GX command — malloc churn (~7 % in
  malloc/free); a SmallVec-style inline buffer would remove it.
- `audio::mixer::tick` + `Timers::tick` are called once per instruction
  pair with tiny cycle counts; batching per scanline would cut loop
  overhead.

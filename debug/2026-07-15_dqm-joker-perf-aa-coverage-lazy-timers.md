# DQM Joker slowness: per-pixel AA clipping allocs + per-cycle timer ticks

Date: 2026-07-15
Status: **Fixed** — 61 → 128 fps in the heaviest 3D scene measured, output
verified pixel-identical and timing-identical before/after each change.

## Symptom

Dragon Quest Monsters: Joker (AJRP) felt slow with visible button-to-screen
lag. Headless bench: ~62 fps at the 3D title, dipping to ~50 in the intro —
right at the 59.83 fps real-time budget, so the SDL frontend (which never
skips frames, it just runs slow-motion) felt laggy. HeartGold hadn't exposed
this because its overworld is mostly 2D; DQM Joker is full-screen 3D with
DISP3DCNT anti-aliasing enabled.

## Root cause 1 — heap-allocating f64 polygon clip per pixel (~40% of runtime)

With AA on (DISP3DCNT bit 4), `triangle_pixel_coverage_and_edges` ran for
**every rasterized pixel** and did a full Sutherland-Hodgman clip of the
pixel box against the triangle: one `vec![]` plus three fresh `Vec`s from
`clip_polygon_to_edge`, all f64. `sample(1)` showed the top leaves were
`postfx::apply`, `RawVec::grow_one`, and a wall of malloc/free.

Fix (`gpu3d/raster/triangle.rs`), bit-identical by construction:

1. **Interior fast path**: if all 4 pixel-box corners are inside the
   triangle, the clip provably returns the whole box → coverage 31, hints 0.
   Interior pixels are the overwhelming majority.
2. **Stack ping-pong buffers** for true edge pixels: a convex n-gon clipped
   by a half-plane yields ≤ n+1 vertices, so `[(f64,f64); 16]` is oversized;
   `clip_polygon_to_edge` now writes into a caller buffer and returns a len.

Result: 61 → 101.6 fps, framebuffer hashes identical over 240 frames.

## Root cause 2 — 4-timer walk twice per instruction pair (~20% of runtime)

`run_cycles` calls `tick_timers` every instruction-pair iteration (~33M/s),
which walked 4 timers × 2 clock domains. The `any(enabled)` early-out never
fires in DQM Joker (sound timers always run).

Fix (`timer.rs`): **lazy ticking with an exact-overflow countdown**.

- `tick_lazy(cycles)`: accumulate into `pending_cycles`; only flush when
  `pending >= countdown`. Two adds + compare on the hot path.
- `countdown` = exact cycles to the earliest possible overflow among enabled
  non-cascade timers (`((0x10000 - counter) << prescaler_shift) -
  prescaler_counter`). Cascade timers only move when their driver overflows,
  which is itself a flush point — so IRQs land in **the same iteration** an
  eager tick would have fired them: timing is unchanged, not approximated.
- `read_counter` folds `pending_cycles` in on the fly (the IO read path is
  `&SharedState`); no overflow can hide in the pending window, so a plain
  add is exact. `write_reload`/`write_control` sync before applying and
  recompute the countdown.
- New fields are `#[serde(skip)]` → save-state format unchanged, old states
  load and self-heal (countdown 0 forces a flush on first tick).

Result: 101.6 → 128 fps, hashes still identical (any IRQ shifted by even one
instruction would desync the whole replay — the hash check doubles as a
timing regression test).

## Measured

| Scene | before | after |
|---|---|---|
| DQM Joker 3D intro cutscene | 61.0 fps (min 50) | **128.0 fps** (min 122) |
| HGSS boot/intro | ~78 fps | **233.6 fps** |

668 nds-core + 7 frontend tests pass. Old save states load unchanged.

## Post-fix profile (for the next round, if ever needed)

At 128 fps: 3D raster ~35% (biggest remaining: per-texel
`VramRouter::read_texture_image/palette` bank dispatch — a flat per-frame
texture/palette snapshot would cut this), `run_cycles` loop bookkeeping ~25%
(per-iteration scheduler/pump/refresh_level_irqs checks), CPU interp ~18%,
2D ~13% (same per-pixel router pattern in `read_engine_a_bg`), audio ~4%.

## Repro / tooling

Headless bench + pixel-hash driver: scratchpad `ndsh.py` (speaks the harness
wire protocol: `[total_len:u32le][json_len:u32le][json][blob]`; note
`save_state`/`load_state` pass state bytes as the blob, not a path).
Profiler: plain macOS `/usr/bin/sample <pid>` while the harness steps.
States (scratchpad): `dqm-title.state`, `dqm-menu.state`,
`dqm-newgame.state` (name entry), `dqm-postname.state`, `dqm-cutscene.state`
(in-engine 3D cutscene — the benchmark scene).

# Concept: Rasterization (background for Phase 7)

Phase 6 ends with a `Vec<ScreenPolygon>` — each polygon a small flat shape on the screen with per-vertex attributes (color, depth, texture coords, w). Phase 7's job is to turn that list into a 256×192 pixel framebuffer.

This doc explains how. Same role as `3d-graphics-basics.md` was for Phase 6 — read this and the Phase 7 implementation reads as "obvious, with these specific knobs."

## 1. The job

Given a polygon with N vertices at known pixel positions, each carrying its own color / depth / texture coords:

```
       V0 (10, 20)   color=red,   z=0.5,  uv=(0,0)
         •
        / \
       /   \
      /     \
     /       \
    •─────────•
   V1 (5,40)   V2 (40, 40)
   color=blue   color=green
   z=0.3        z=0.7
   uv=(0,1)     uv=(1,1)
```

For every pixel `(x, y)` inside the polygon's outline:

1. Figure out which fraction of the polygon's interior this pixel sits at.
2. Use that fraction to interpolate each per-vertex attribute (color, z, u, v, …).
3. Do a depth test against the depth buffer.
4. Fetch the texel at `(u, v)` from VRAM.
5. Combine texel × color → pixel color.
6. Alpha-test, alpha-blend, post-effects.
7. Write to the framebuffer.

The "fraction of the polygon's interior" idea is **barycentric coordinates**. For a triangle with vertices `V0, V1, V2`, every point inside is uniquely expressible as `α·V0 + β·V1 + γ·V2` with `α + β + γ = 1` and all three non-negative. The pixel's interpolated value of attribute `A` is just `α·A_0 + β·A_1 + γ·A_2`.

In practice rasterizers don't compute barycentrics directly — they walk edges along scanlines and step attributes by precomputed gradients. The end result is the same.

## 2. The per-pixel pipeline

For each polygon, for each pixel:

```
                  ┌─ edge test (is this pixel inside?) ─ no → skip
                  │  yes → continue
                  ▼
        ┌─ interpolate depth at (x, y) ─┐
        │                               │
        ▼                               │
  depth test: is new_z < zbuf[x,y]?     │
        │                               │
   yes → │                              │
        ▼                               │
  interpolate color, U/W, V/W, 1/W,     │
        fog_factor at (x, y)            │
        │                               │
        ▼                               │
  texture fetch: VRAM[texel(U, V)]      │
        │                               │
        ▼                               │
  combine: texel × color (or decal /    │
            replace / blend per mode)   │
        │                               │
        ▼                               │
  alpha test: alpha ≥ ref?              │
        │                               │
        ▼                               │
  alpha blend (if translucent)          │
        │                               │
        ▼                               │
  write color → framebuffer[x,y]        │
  write z → zbuffer[x,y]                │
  write polygon ID → id_buffer[x,y]     ◄┘  (used for edge marking later)
```

Everything else — perspective-correct interpolation, fog, edge marking, anti-alias, toon — is variants on "interpolate this thing across the polygon" or "post-process the loop output."

## 3. Two ways to walk the polygon

Iterating which pixels a polygon covers can be done two ways:

**Scanline.** For each horizontal row `y` from `y_min` to `y_max`:
- Walk the polygon's edges to find `(x_left, x_right)` for this row.
- Walk pixels from `x_left` to `x_right`.
- Each step (across rows and pixels) increments attribute values by precomputed deltas.

**Tile.** Chunk the screen into 8×8 or 16×16 tiles; for each tile that overlaps the polygon, test every pixel.

```
Scanline                            Tile
────────                            ────

  • ─→ → → → →                       ┌──┬──┐
  • ─→ → → → →                       │  │  │
  • ─→ → → → →                       ├──┼──┤
  • ─→ → → → →                       │  │  │
                                     └──┴──┘
  iterate y, then x                  iterate tiles, test pixels
```

Scanline wins on a single-issue CPU rasterizer (sequential memory access, no pixel-test overhead off the polygon). Tile wins on modern GPUs where you have hundreds of pixel shaders and want cache locality on the tile-sized framebuffer block.

**We're doing scanline.** That's what the NDS GPU does in silicon, and it maps cleanly to our existing per-polygon loop.

### Scanline anatomy

For a triangle, sort the three vertices by `y`. Then split the triangle into a "top half" (between `V_top.y` and `V_mid.y`) and a "bottom half" (between `V_mid.y` and `V_bot.y`). Each half has a "long edge" (top to bottom) and a "short edge" (top to mid or mid to bot).

```
       V_top
         •
        ╱ ╲
       ╱   ╲        ← top half: scanlines from V_top to V_mid
      ╱     ╲
     •───────╳      ← V_mid + interpolated point on long edge
      ╲     ╱       ← bottom half: scanlines from V_mid to V_bot
       ╲   ╱
        ╲ ╱
         •
       V_bot
```

For each scanline `y`:
- Compute `x_left, x_right` by linear-interpolating along the two active edges.
- For each attribute, compute the value at `x_left` and the gradient `(value_right - value_left) / (x_right - x_left)`.
- Walk pixels from `x_left` to `x_right`, stepping each attribute by its per-pixel gradient.

Quads on NDS we split into two triangles for rasterization — simpler than handling 4-vertex edges directly, and the silicon does the same.

After clipping a polygon may have up to 10 vertices. We fan-triangulate it (V0-V1-V2, V0-V2-V3, V0-V3-V4, …) and rasterize each triangle.

## 4. Perspective-correct interpolation — the one tricky bit

If you naively linear-interpolate `U` and `V` across the screen, **textures look wrong on slanted polygons**. This is the famous "wavy floor" artifact in PS1 games — they couldn't afford perspective correction.

### Why naive interpolation fails

Imagine a long floor stretching into the distance. It's a quad with U-coords 0 at the near end and 1 at the far end. Linear interpolation of U across the screen says: U = 0.5 halfway between the near edge and the horizon line.

But the perspective divide (÷ w) at the end of geometry made the *far* half of the quad take up much less screen space than the *near* half. So screen-Y halfway from near to horizon corresponds to a 3D position much closer to the *near* end, not 50% along the quad. The texture should still read at U ≈ 0.2, not 0.5.

Visually: textures appear to slide and warp as the camera moves, with "creases" along triangle edges where the interpolation disagrees with reality.

### The fix

Instead of interpolating U and V directly across the screen, interpolate **three things linearly**:

- `U / W`
- `V / W`
- `1 / W`

Then at each pixel, recover the true `U` and `V`:

```
U_pixel = (U/W)_interpolated / (1/W)_interpolated
V_pixel = (V/W)_interpolated / (1/W)_interpolated
```

This is **perspective-correct interpolation**. Costs a divide per pixel but the result is geometrically right.

### Worked example

Near vertex: U = 0, W = 1.
Far vertex: U = 1, W = 10 (10× further from camera).

Naive interpolation says screen-midpoint = U = 0.5.

Correct interpolation:
- `U/W` at near = 0, `U/W` at far = 0.1. Midpoint: 0.05.
- `1/W` at near = 1.0, `1/W` at far = 0.1. Midpoint: 0.55.
- True `U` at midpoint = 0.05 / 0.55 ≈ **0.091**.

So at screen-midpoint, we read from U ≈ 0.09, not 0.5. The texture stays anchored to the underlying 3D geometry instead of stretching uniformly across the screen.

### Why our `ScreenVertex` keeps `w`

Phase 6's `ScreenVertex` struct carries the original `w` along with the post-divide screen coords:

```rust
pub struct ScreenVertex {
    pub screen_x: i32,    // post-divide pixel coord
    pub screen_y: i32,
    pub depth_z: i32,
    pub w: i32,           // ← kept specifically for perspective correction
    pub color: u16,
    pub tex: [i16; 2],
}
```

That `w` is what the rasterizer divides into U and V to get the perspective-corrected values per pixel.

### What about color and depth?

Color and depth interpolation are NDS-specific choices:

- **Color**: most rasterizers linearly interpolate color in screen space and call it done. Slight artifacts on very large polygons but invisible in practice. NDS does linear.
- **Depth**: the NDS lets you pick **Z-buffer** (post-perspective depth, depth = z/w in NDC) or **W-buffer** (depth = w directly) per frame via `DISP3DCNT.depth_buffer_mode`. Each has different precision distribution — Z-buffer has more precision near the camera; W-buffer has uniform precision. Games pick based on what looks better.

## 5. Depth testing

The **depth buffer** (or "Z-buffer") is a `256 × 192` array of "what's the closest depth I've seen at this pixel so far?". Before writing a new pixel:

```rust
if new_depth < depth_buffer[pixel] {
    framebuffer[pixel] = new_color;
    depth_buffer[pixel] = new_depth;
}
// else: the new pixel is behind something already drawn; skip the write.
```

This is how you draw polygons in any order and get the right occlusion. Without it, you'd have to sort polygons back-to-front each frame (the "painter's algorithm"), which is expensive and breaks on intersecting geometry.

At the start of each frame, the depth buffer gets cleared to the "max depth" value from `CLEAR_DEPTH`. The framebuffer gets cleared to the color from `CLEAR_COLOR`. Both happen automatically — games can also set background image / fog parameters at clear time.

### Translucent polygons

The NDS draws **opaque polygons first, translucent polygons after**. Translucent polygons read the depth buffer but only write to it if `POLYGON_ATTR.depth_update_for_translucent` is set. This means:

- Multiple translucent polygons at the same depth blend cumulatively (since the second one isn't occluded by the first if the first didn't update depth).
- Translucent polygons in front of opaque polygons still get occluded properly (because they still *read* depth).

This is the standard "draw opaque, then translucent back-to-front" idea, just with the order partly enforced by hardware.

## 6. Texture fetch

The polygon's `TEXIMAGE_PARAM` (set by `TEX_IMAGE_PARM` GX command) describes the active texture:

- VRAM base offset (where the texture lives in the "texture image" target — Phase 3's VRAM router routes banks to this target)
- Width and height (powers of 2, 8 to 1024 pixels)
- Color format: one of 8 formats (4bpp, 8bpp, A3I5, A5I3, 4×4 block compressed, direct color, etc.)
- Wrap mode (clamp / repeat / mirror)

The rasterizer takes the perspective-corrected `(U, V)`, masks/clamps/mirrors per the wrap mode, samples a texel from VRAM, and combines with the per-vertex color per the polygon's mode (modulate / decal / shadow / etc).

For Phase 7 we'll need to wire VRAM bank routing for the texture target — Phase 3 already built the router, we just consume it.

## 7. NDS-specific post-effects

Beyond the basic pipeline, the NDS rasterizer adds five post-rasterization passes, each controlled by `DISP3DCNT` and per-polygon attributes:

### Edge marking (`DISP3DCNT.edge_mark`)

Each polygon has a **6-bit ID** in `POLYGON_ATTR`. During rasterization we write the polygon ID to an `id_buffer` alongside the framebuffer. After all polygons are drawn, a final pass:

- For each pixel, compare its polygon ID to its four neighbors (N/S/E/W).
- If any neighbor has a different ID, this pixel is an edge — tint it with the corresponding entry from the 8-entry `EDGE_COLOR` table.

Net effect: every silhouette gets a configurable colored outline. Gives a clean cel-shaded / pop-up-book look. Used by games like *Trauma Center* for the comic-style UI.

### Fog (`DISP3DCNT.fog_enable`)

For each pixel after texturing:

- Look up the pixel's depth in a 32-entry **`FOG_TABLE`** to get a density value in 0..127.
- Blend the pixel color toward `FOG_COLOR` by that density.

The fog table is indexed by depth, so games define non-linear fog falloff (e.g. sharp near the far plane, gentle elsewhere).

### Anti-aliasing (`DISP3DCNT.antialias`)

When enabled, the rasterizer computes **fractional coverage** for pixels on triangle edges — what fraction of the pixel is actually inside the polygon. At the end, edge pixels get blended with their neighbor (the one across the polygon edge) by the coverage value, which softens jaggies.

This is not full multisampling — it's coverage-based with a single sample per pixel. Cheap, looks good enough on a 256×192 screen.

### Toon / highlight shading (`DISP3DCNT.shading_mode`)

When enabled, the **red channel** of the per-vertex color (after lighting + texture combine) gets re-mapped through a 32-entry `TOON_TABLE`:

- **Toon mode**: red channel replaces all three channels (gives banded "cel-shaded" steps).
- **Highlight mode**: red channel gets added to all three channels (creates bright specular highlights).

Both are cheap cel-shading variants used by anime-style games.

### Capture (`DISPCAPCNT` + Engine A)

This is technically a 2D-engine feature (Phase 3 left it stubbed), but the 3D framebuffer is one of its source options. When enabled, Engine A captures the 3D framebuffer (or a blend of 3D + 2D layers) into a VRAM bank — so games can post-process previous frames (motion blur, screen distortion, picture-in-picture).

## 8. How the 3D framebuffer reaches the screen

The 3D engine's output is a **256×192 BGR555 framebuffer** kept in its own internal storage. It does **not** go directly to the LCD.

Engine A composites it as one of its BG layers. Specifically:

- `DISPCNT` bit 3 = **"BG0 source is 3D"** — when set, Engine A's BG0 layer reads the 3D framebuffer instead of tile data. The BG0 priority + blend rules still apply (so 2D OBJs above it, fog effects through the blend pipeline, etc.).
- This is why the 3D engine is "Engine A only" — Engine B has no BG0-from-3D source.

For Phase 7 our integration point is exactly the same shape as Phase 3's BG renderers: a function `render_3d_scanline(line: u16) -> [Pixel; 256]` that Engine A's compositor calls when DISPCNT bit 3 is set. The actual rasterization of all polygons can happen at SWAP_BUFFERS (build a full 256×192 buffer at frame start) and the scanline function just reads from it.

## 9. Implementation order for Phase 7

| Step | Module | What it does |
|---|---|---|
| 1 | `gpu3d/raster/edge.rs` | Scanline edge walker: given two vertices, step `x_left`/`x_right` and per-attribute gradients per row. |
| 2 | `gpu3d/raster/interp.rs` | Per-pixel attribute interpolation including the perspective-correct U/V trick. |
| 3 | `gpu3d/raster/depth.rs` | Z-buffer and W-buffer modes, depth test, depth clear. |
| 4 | `gpu3d/raster/texture.rs` | All 8 texture formats, wrap modes, palette lookup. Reads VRAM via the existing router. |
| 5 | `gpu3d/raster/mod.rs` | Per-polygon: triangulate, sort by y, scanline-walk each triangle through the inner-loop pipeline. |
| 6 | `gpu3d/raster/postfx.rs` | Edge marking, fog, AA, toon. Each as a separate pass over the framebuffer. |
| 7 | `lib.rs` | Call `rasterize_frame` at SWAP_BUFFERS; expose the 3D framebuffer to Engine A. |
| 8 | `gpu2d/engine_a` | Add the "BG0 source is 3D" path. |

Total budget estimate: 1,500-2,000 lines, similar to Phase 6.

## 10. Mental model

> **The rasterizer is a per-pixel loop inside a per-polygon loop. Each iteration interpolates vertex attributes to one pixel, does a depth test, does a texture fetch, blends, and writes one framebuffer pixel. Everything else — perspective correction, edge marking, fog, AA, toon — is variants on "interpolate this across the polygon" or "post-process the inner-loop output."**

The single most subtle piece is perspective-correct interpolation of texture coordinates. Skip that and textures look like 1996 PS1 games. Implement it correctly with the `U/W, V/W, 1/W` linear interp + per-pixel divide and you get the same flat textured perspective that every modern 3D engine produces.

The other four post-effects (edge mark, fog, AA, toon) are completely independent of each other and of the inner loop — they're individual passes that operate on the (color, depth, polygon-ID) buffer triple after the main rasterization is done. We can wire any subset of them and the others stay zero-cost-when-disabled.

That's everything Phase 7 needs to do.

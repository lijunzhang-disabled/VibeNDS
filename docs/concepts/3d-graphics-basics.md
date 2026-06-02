# Concept: 3D graphics pipeline (background)

Background reading for Phase 6 / Phase 7. The NDS 3D engine is a *very* classic fixed-function pipeline — late 1990s desktop-GPU shape, scaled down. Everything in this doc applies to OpenGL 1.x, Direct3D 7, the PS1, the Saturn, and the NDS, with only the bit-widths and feature-flags changing between them.

If you already know "model → view → projection → clip → NDC → screen," skim and skip. If those words are unfamiliar, this is the doc that makes Phase 6's matrix math read as obvious instead of arbitrary.

## 1. The job

We have a list of 3D points (called **vertices**), grouped into **primitives** (almost always triangles). Each vertex has a position `(x, y, z)` and maybe other attributes like color or texture coordinates. We have a virtual camera somewhere in 3D space looking in some direction.

The job: produce the 2D image the camera "sees" — a `width × height` framebuffer of colored pixels.

The classic pipeline does this in two stages:

```
                       ┌─────────────────────────────┐
   3D vertices  ────►  │  GEOMETRY (this doc, P6)    │  ────►  2D polygons in
   + camera             │                             │          screen pixels +
   + lights             │   transform → light → clip  │          per-vertex attrs
                        │   → viewport                │
                        └─────────────────────────────┘
                                                          │
                        ┌─────────────────────────────┐   │
                        │  RASTERIZATION (P7)         │ ◄─┘
                        │                             │
                        │   for each polygon:         │
                        │     for each scanline:      │
                        │       for each pixel:       │
                        │         interpolate, depth- │
                        │         test, texture, blend│
                        └─────────────────────────────┘
                                  │
                                  ▼
                              framebuffer
```

Phase 6 covers the top box. Phase 7 covers the bottom.

## 2. Coordinate spaces (the heart of the pipeline)

A vertex's `(x, y, z)` numbers are meaningless on their own — they only have meaning *relative to some space*. The geometry pipeline moves vertices through **five coordinate spaces** in sequence:

```
  object space  ──model────►  world space  ──view────►  view (camera) space
       │                                                       │
       │     "where am I in the model?"                        │
       │     e.g. "10 cm above the spaceship's origin"         │
       │                                                       │
                            "where am I in the scene?"         │
                            e.g. "(120, 50, -800) in the world"│
                                                               │
                                                "where am I    │
                                                relative to the│
                                                camera?"       │
                                                e.g. "30 m     ▼
                                                ahead of cam"
                                                projection
                                                   │
                                                   ▼
                                            clip space
                                                   │
                                       perspective divide (÷W)
                                                   │
                                                   ▼
                                       normalized device coords (NDC)
                                       "[-1..+1] cube"
                                                   │
                                            viewport transform
                                                   │
                                                   ▼
                                            screen space (pixels)
```

Each `─Xform────►` arrow is a **matrix multiply**. The whole pipeline is "multiply by a stack of matrices, then divide by w, then scale to pixels."

That's the entire thing. Everything else is variants of who provides which matrix and what extra attributes get carried along.

## 3. Why matrices, and why 4×4

A 3×3 matrix can rotate, scale, and shear a 3D point. What it **cannot** do is translate — moving a point by `(tx, ty, tz)` requires *adding* a vector, not multiplying by a matrix.

The trick: pretend every 3D point `(x, y, z)` is actually a 4D point `(x, y, z, 1)`. Now a 4×4 matrix can encode translation in its rightmost column:

```
┌  1   0   0   tx  ┐   ┌ x ┐     ┌ x + tx ┐
│  0   1   0   ty  │ × │ y │  =  │ y + ty │
│  0   0   1   tz  │   │ z │     │ z + tz │
└  0   0   0   1   ┘   └ 1 ┘     └   1    ┘
```

This is **homogeneous coordinates**: the extra "1" is the w-coordinate. It's just a bookkeeping trick that turns translation into a multiply, which means the whole transformation chain becomes one big matrix multiply.

The NDS GPU uses 4×4 fixed-point matrices throughout. So do all classical GPUs.

### Rotation example

A rotation around the Z axis by angle θ:

```
┌  cos θ  -sin θ   0   0  ┐
│  sin θ   cos θ   0   0  │
│   0       0      1   0  │
└   0       0      0   1  ┘
```

Multiply this by `(1, 0, 0, 1)` and you get `(cos θ, sin θ, 0, 1)` — the unit vector along X, rotated to angle θ. Pure geometry.

## 4. The three matrices

The geometry pipeline conventionally composes three matrices into one:

```
  clip = projection  ×  view  ×  model  ×  vertex
              ▲             ▲            ▲
              │             │            │
       "how does a 3D      "where's      "where's
        scene get          the           the model
        squashed onto      camera?"      placed in
        a 2D image?"                     the world?"
```

- **Model matrix**: places one object in the world. A spaceship at (120, 50, -800), rotated 30° around Y, is a model matrix doing translate + rotate.
- **View matrix**: pretends the camera is at the origin looking down -Z by moving the whole world the *opposite* way. If the camera is at (0, 5, 100), the view matrix translates everything by (0, -5, -100).
- **Projection matrix**: squashes a 3D frustum (truncated pyramid) into a `[-1, +1]³` cube. This is the only one of the three that does anything non-rigid — it makes distant things appear smaller.

For an emulator's perspective, the game has already computed these matrices and is just sending them to the GPU. We don't synthesize matrices; we receive them. But knowing what they *do* is what lets us write tests that say "multiplying this matrix by this vertex should give that result."

### NDS specifics

The NDS doesn't separate "model" from "view." It has:

- **Projection matrix** (`MTX_MODE = 0`, 1-deep stack) — the projection.
- **Position matrix** (`MTX_MODE = 1`, 32-deep stack) — combined model×view, what other APIs call "modelview."
- **Position+Vector matrix** (`MTX_MODE = 2`, 32-deep stack, kept in lockstep with position) — also tracks the inverse-transpose for normal vectors during lighting.
- **Texture matrix** (`MTX_MODE = 3`, 1-deep stack) — transforms texture coordinates.

A typical game push/pop pattern:

```
MTX_MODE   1                     # select position matrix
MTX_LOAD   <view_matrix>         # set the view (whole-scene basis)
                                  # repeated per object:
MTX_PUSH                         # save current position matrix
MTX_MULT   <model_matrix>        # post-multiply by this object's model matrix
<send vertices for this object>
MTX_POP                          # restore — back to pure view matrix
```

That's why the position stack is 32-deep: nested object hierarchies (e.g. a character whose hand contains a sword that contains a gem) can push 5+ matrices deep without overflowing.

## 5. The pipeline step by step

For each vertex the game submits:

### 5a. Multiply by the current matrix

```
clip_pos = projection × position × vertex_pos
```

This is one 4×4 × 4-vector multiply (matrix mul matrix could be precomputed once per primitive, but the NDS GPU does it per-vertex). Output: a 4D **clip-space** position with potentially weird-looking `(x, y, z, w)` values.

### 5b. Lighting (if enabled for this polygon)

Compute the per-vertex color from:

- A normal vector for the vertex (transformed by the position+vector matrix's rotational part)
- Up to 4 directional light sources, each with diffuse + specular color
- A material's ambient + diffuse + specular + emissive coefficients
- A "reflective" lookup table that approximates the Phong specular highlight as a 32-entry LUT (real Phong is `(reflection · view)^shininess`; the NDS approximates this with a table indexed by the dot product)

Output: a single RGBA color per vertex. (Alternatively, the game can submit colors directly via `VTX_COLOR`, skipping lighting.)

### 5c. Texture coordinate transform

If the polygon uses a texture, the texture matrix transforms the per-vertex `(s, t)` UV coordinates. Most games just use the identity matrix here.

### 5d. Polygon assembly

Vertices stream in. The primitive type (`BEGIN_VTXS`) decides when a polygon is "complete":

- `TRIANGLES`: every 3 vertices = 1 triangle
- `QUADS`: every 4 vertices = 1 quad
- `TRIANGLE_STRIP`: vertices N, N+1, N+2 form a triangle; alternating winding
- `QUAD_STRIP`: similar shape for quads

Once a polygon is complete it gets a `POLYGON_ATTR` payload (alpha, polygon ID, fog enable, lighting enable, cull mode) and goes to the next stage.

### 5e. Clipping

Some vertices might land *outside* the camera's view frustum — too far left/right/up/down, or behind the camera, or beyond the far plane. We can't naively rasterize those; the math breaks down at `w ≤ 0`.

The fix: **Sutherland-Hodgman clipping** against the six planes of the canonical clip volume:

```
-W ≤ x ≤ +W       (left and right planes)
-W ≤ y ≤ +W       (bottom and top planes)
   0 ≤ z ≤ +W     (near and far planes)
```

(`W` here is the w-coordinate of the homogenized vertex. These planes define a 4D frustum in clip space.)

For each plane, classify each polygon vertex as inside or outside. The algorithm produces new vertices at the plane intersections — a triangle straddling the near plane becomes 1 or 2 triangles fully inside it. This is **the** classic graphics algorithm; the implementation is ~50 lines of code.

Output: 0 or more "clip-space" polygons, all with `w > 0`.

### 5f. Perspective divide

For each surviving clip-space vertex `(x, y, z, w)`:

```
x_ndc = x / w
y_ndc = y / w
z_ndc = z / w
```

This is the "magic" that makes 3D look 3D — dividing by w means distant things (with large w) get smaller x/y values, while close things (small w) get bigger. **NDC** = Normalized Device Coordinates, where the viewport is `[-1, +1]³`.

It's also where the fixed-function pipeline becomes inherently non-linear: everything before this point is linear matrix algebra; everything after this point is in screen space. (This is why texture coordinate interpolation across a triangle is fiddly — it has to undo the perspective divide. See [perspective-correct interpolation](https://en.wikipedia.org/wiki/Texture_mapping#Perspective_correctness) for the rabbit hole.)

### 5g. Viewport transform

Convert NDC to screen pixels:

```
viewport_w = x2 - x1 + 1
viewport_h = y2 - y1 + 1
screen_x = x1 + (x_ndc + 1) * (viewport_w / 2)
screen_y = (191 - y2) + (1 - y_ndc) * (viewport_h / 2)
```

On the NDS, `Y1` is the bottom edge and `Y2` is the top edge. The hardware
uses the inclusive viewport size, so polygons can reach one pixel beyond
`X2` and `Y1`; the physical framebuffer still clips to 256x192.

Output: screen-space `(x, y, z, w, s, t, r, g, b, a)` per vertex, ready for rasterization.

## 6. What the geometry stage produces

A list of **polygons** (triangles or quads), each pointing to **vertices** with the post-transform attributes. The exact NDS limits:

- **2048 polygons** per frame
- **6144 vertices** per frame

If a frame's geometry exceeds either, the rest are dropped. `GXSTAT[15]` is
the matrix stack overflow/underflow flag; it is not a polygon-list overflow
bit.

The polygon list is **double-buffered**: while frame N rasterizes, frame N+1's geometry is being built. The `SWAP_BUFFERS` command (GX cmd `0x50`) atomically swaps the two buffers at the next frame boundary.

## 7. What the rasterizer does with it (Phase 7 preview)

For each polygon in the swapped list, for each pixel it covers:

```
1. Interpolate from the polygon's vertices:
     - depth (Z or W, per DISP3DCNT)
     - color (R, G, B, A)
     - texture coords (S, T)  — perspective-correct
     - fog factor
2. Texture fetch — sample the texel at (S, T) from VRAM
3. Combine with the interpolated color per the polygon's mode (modulate, decal, etc.)
4. Depth test against the framebuffer's Z-buffer
5. Alpha test, alpha blend (if translucent)
6. Edge marking, fog, toon/highlight as post-effects
7. Write to the 3D framebuffer
```

The 3D framebuffer is then composited by **Engine A** as one of its BG layers (when `DISPCNT` bit 3 is set), and shows up on the top screen alongside any 2D BGs.

## 8. NDS-specific quirks worth knowing now

The NDS GPU is a *very* faithful fixed-function pipeline with a few oddities you'll meet in Phase 6:

| Quirk | Why it's weird | Where it'll bite us |
|---|---|---|
| Fixed-point everywhere | No FPU. Matrices are 1.19.12 fixed-point (`i32`), intermediate products use `i64`. | Every matrix multiply has to track overflow. |
| Quads as a first-class primitive | Most modern GPUs only do triangles. NDS GPU does quads natively, so we have to handle 4-vertex polygons through clipping. | Sutherland-Hodgman on quads outputs general n-gons; we either re-triangulate or keep them as polygons. |
| 32-deep position stack, 1-deep projection stack | Asymmetric. Games push the position stack constantly; projection is set once per frame. | `MTX_PUSH`/`MTX_POP` on projection are no-ops above depth 1 (with overflow flag). |
| W-buffering option | Most GPUs do Z-buffering. NDS lets you pick Z or W depth per frame via `DISP3DCNT`. | Affects fog and depth-test math in Phase 7. |
| Polygon ID for edge marking | Each polygon gets a 6-bit ID; pixels where IDs differ at the edge get tinted by an 8-entry edge color table. | Edge marking is a post-rasterization pass that needs the ID buffer. |
| Toon / highlight shading modes | Optional cel-shading lookup table that re-maps the red channel. | A `POLYGON_ATTR.mode` flag we'll plumb through but mostly leave for Phase 9 polish. |

## 9. Mental model

> **The geometry pipeline is one big matrix multiply that lands per-vertex data in screen space, plus a clip step that handles vertices the multiply can't represent.**

When we sit down to implement Phase 6, every command in the GX command table maps to one of:

- "Update a matrix" (load / mult / push / pop)
- "Receive a vertex" (transform it through the current matrices)
- "Receive vertex metadata" (color, normal, texcoord — they apply to the *next* `VTX_*`)
- "Start / end a primitive" (`BEGIN_VTXS` / `END_VTXS`)
- "Swap buffers" (`SWAP_BUFFERS`)
- "Set a viewport / fog / light / material parameter"

That's it. ~50 commands, each falling into one of those six buckets. The whole "3D engine" is administrative wiring around per-vertex matrix algebra.

## 10. Recommended further reading (if any of this is fuzzy)

- **Eric Lengyel, "Foundations of Game Engine Development Vol 1: Mathematics"** — chapter 4 covers homogeneous coords + projection matrices clearly.
- **Tom Forsyth, "Linear-Speed Vertex Cache Optimisation"** — old GPU pipeline blog post that's still the best one-page explainer of why pre/post-transform vertex caching exists.
- **GBATEK §"DS 3D Engine"** — the NDS-specific reference. After reading this doc, skim the GX command table; every command should map to one of the six buckets above.
- **Real-Time Rendering (4th ed.)** chapters 2-3 — the modern canonical reference, more depth than we need but the diagrams are unbeatable.

## 11. What's coming in Phase 6

With this background, the Phase 6 plan (in `PLAN.md`) reads as:

| Plan item | What we now know it means |
|---|---|
| GXFIFO at `0x04000400` | Where GX commands queue. Six buckets above. |
| 4 matrix stacks (1+32+32+1) | Storage for the three matrices in §4 + the texture matrix. |
| Vertex pipeline `VTX_16/10/XY/...` | §5a transformation, repeated per submitted vertex. |
| Per-vertex lighting | §5b — 4 lights, ambient/diffuse/specular, table-based Phong. |
| Polygon assembly | §5d — group vertices into tris/quads per the active `BEGIN_VTXS`. |
| Sutherland-Hodgman 6-plane clip | §5e — clip against the canonical clip volume. |
| Viewport + perspective divide | §5f + §5g together. |
| `SWAP_BUFFERS` | §6 atomic swap of the polygon/vertex double buffers. |

Every line of the Phase 6 plan now has a "why" attached.

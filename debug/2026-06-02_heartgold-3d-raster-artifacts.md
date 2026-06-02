# HeartGold title scene: 3D raster artifact fixes

Date: 2026-06-02
Status: **Improved, not fully finished**

## Symptom

Pokemon HeartGold reached the Game Freak/title 3D scene, but the image was
unstable: random polygon flashes appeared across frames, and the title view
still had residual artifacts compared with expected DS output.

Earlier boot work had already moved the game past the black screen into visible
graphics. This pass focused on local 3D rules that could plausibly affect the
remaining flashing and edge artifacts.

## Reference use

Direct reference-emulator implementation copying in this batch: **0 fixes**.

The fixes below came from public hardware documentation and local tests:

- GBATEK DS 3D polygon/display-control notes for translucent depth, edge
  marking, and anti-aliasing edge cases.
- ndsdoc geometry command notes for `POLYGON_ATTR` timing and polygon assembly.
- Local unit tests around raster buffers, zero-dot polygons, shadow mode, and
  clipping.

No melonDS or other emulator code was used as an implementation source for this
batch.

## Bug 1: translucent polygons overwrote opaque depth when they should not

### Broken behavior

The rasterizer wrote a new depth value for translucent fragments in the normal
path. On DS, translucent polygons only update the depth buffer when
`POLYGON_ATTR` bit 11 is set. Without that bit, they blend into the color buffer
but leave the existing depth.

### Why this matters

If translucent fragments incorrectly update depth, later geometry can be
rejected against the wrong surface. In a title scene with layered translucent
effects, this can create flicker or missing pieces depending on draw order.

### Fix

Added coverage for both sides of the rule:

- `test_translucent_polygon_does_not_update_depth_without_attr_bit11`
- `test_translucent_polygon_updates_depth_with_attr_bit11`

The existing implementation already mostly followed the correct path after the
current raster pass, so these tests lock down the behavior.

## Bug 2: translucent overlays changed the edge-mark polygon ID

### Broken behavior

When a translucent polygon drew over an opaque polygon, the code preserved the
opaque pixel's edge-mark flag but still overwrote the pixel's polygon ID.

Edge marking colors are selected from the polygon ID group. Preserving the edge
flag but replacing the ID meant the post-effect could outline the opaque object
with the translucent overlay's edge color.

### Why this matters

This can make title-scene outlines appear with the wrong color or flash as
translucent effects pass over already drawn opaque geometry.

### Fix

At the wire-line, point, and filled-scanline write sites, `id_buffer` is now
left unchanged when the fragment is in the "preserve opaque edge" case:

```rust
if !preserve_edge {
    rast.id_buffer[idx] = poly_id;
}
```

The test `test_translucent_overlay_preserves_opaque_edge_mark_flag` now asserts
both the rendered edge color and the retained polygon ID.

## Bug 3: opaque zero-dot polygons did not participate in the AA hardware quirk

### Broken behavior

Degenerate polygons that collapse to a single pixel were drawn as normal pixels
even when anti-aliasing was enabled. GBATEK documents a DS quirk where opaque
1-dot polygons effectively disappear under anti-aliasing unless edge marking is
also active.

### Why this matters

Commercial games can use tiny or degenerate polygons during animations. Drawing
opaque zero-dot fragments that hardware would hide can show up as random bright
pixels or small flashing dots.

### Fix

Added a per-pixel `zero_dot_buffer` to the rasterizer. The point path marks
opaque zero-dot fragments, normal line/fill paths clear the flag, and
anti-aliasing hides those pixels when edge marking is disabled.

Important details:

- The buffer is serialized with a serde default so older save states still load.
- Clear paths reset the buffer, including rear-bitmap clear.
- Only opaque zero-dot fragments are tagged. Translucent zero-dot fragments stay
  visible.
- Shadow-mask zero-dot polygons do not tag the AA zero-dot buffer.

New tests:

- `test_antialiasing_hides_opaque_zero_dot_polygon`
- `test_edge_marking_keeps_antialiased_zero_dot_polygon_visible`
- `test_antialiasing_keeps_translucent_zero_dot_polygon_visible`
- `test_shadow_mask_zero_dot_does_not_write_color`
- `test_visible_shadow_zero_dot_draws_only_where_mask_is_clear`

## Bug 4: repeated `POLYGON_ATTR` writes during a list needed explicit coverage

### Broken behavior

No code change was required here, but this was a risky timing rule. Hardware
defers `POLYGON_ATTR` writes made during an active vertex list until the next
`BEGIN_VTXS`; repeated writes keep only the last pending value.

### Fix

Added `test_repeated_polygon_attr_writes_during_list_keep_only_last_pending_value`
to confirm:

- The active list keeps its original polygon attributes.
- Multiple writes during the list do not affect polygons emitted by that list.
- The last pending value is applied when the next list begins.

## Bug 5: clipping contract needed a hardware-limit guard test

### Broken behavior

The clipper comments already described the DS rule: input polygons are triangles
or quads, and after clipping a polygon can have at most 10 vertices. The runtime
path was already fed by the vertex assembler, which only emits triangles/quads,
but there was no test proving a multi-plane-clipped quad remains within the
hardware storage contract.

### Fix

Added `test_clipped_quad_stays_within_hardware_vertex_limit`.

This is a guard test, not a runtime cap. It protects the valid DS input path
without hiding a future bug where the assembler might feed unsupported polygons.

## Verification

Commands run:

```sh
cargo test -p nds-core gpu3d::clip --release
cargo test -p nds-core gpu3d --release
cargo test -p nds-core --release
```

Results:

- `gpu3d::clip`: 7 passed.
- `gpu3d`: 225 passed.
- Full `nds-core`: 507 passed.

Remaining warnings are pre-existing warnings outside this 3D batch.

## Remaining work

This pass improves documented edge cases, but it does not prove HeartGold is
fully correct yet. The likely remaining work is still in 3D raster conformance:

- More visual comparison against a reference capture for the HeartGold title
  scene.
- More tests for line and wireframe anti-aliasing quirks.
- Better coverage for polygon edge rules around lower-right edge exclusion,
  small polygons, and post-effect interactions.
- Dedicated homebrew 3D conformance ROMs once a small test harness is built.


# HeartGold 3D debug progress

Date: 2026-06-02
Status: **In progress**

## Current status

HeartGold is no longer a black-screen boot failure. It reaches a recognizable
title scene, and the latest Desktop screenshot showed the Ho-Oh title art and
`TOUCH TO START` instead of a blank frame or broken layout.

This does **not** mean the NDS 3D renderer is complete. The current goal is
still broader: make local 3D graphics behavior match the NDS geometry docs.
The remaining artifacts still look like real 3D correctness problems, not a
simple frontend scaling issue or a boot-path issue.

Rough completion estimate:

- Boot/runtime path for this title: **mostly past the initial blocker**.
- 3D command and raster behavior: **partially implemented, still needs a
  systematic conformance sweep**.
- Confidence that all local 3D graphic errors are fixed: **not high yet**.

The next work should be driven by small, controlled 3D tests and spec checks,
not by copying another emulator's implementation.

## Important note about melonDS reference use

I briefly looked at melonDS code while investigating polygon ordering. That is
useful as a sanity check, but it is not a substitute for understanding the DS
hardware rules. Translating C++ into Rust would be the wrong way to finish this
project because it would hide the actual invariants we need to own and test.

For this repo, fixes should be justified by:

1. NDS docs / GBATEK behavior,
2. a local minimal test that proves the exact rule,
3. a visible game improvement only as supporting evidence.

The debug notes below are written around root cause and evidence rather than
source-to-source translation.

## Symptom progression

### Initial HeartGold run

Command:

```sh
cargo run --release -p nds-frontend -- --rom ~/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --scale 2
```

Early behavior:

- The game did not boot usefully.
- After boot-path fixes, it reached graphics output.
- Later screenshots showed the Game Freak/title sequence, then the title page,
  but with incorrect rendering and intermittent random polygon flashes.

### Latest visible improvement

Latest screenshot observed on Desktop:

```text
Screenshot 2026-06-01 at 11.57.19 PM.png
```

Visible state:

- Ho-Oh title art appears.
- Background and title prompt are recognizable.
- This confirms the core boot path, 2D display routing, and enough 3D pipeline
  behavior are working to present a commercial title's title scene.

Remaining visible risk:

- Random polygon flashes were still reported before this note.
- Correct title rendering is not yet proven frame-to-frame.
- Without controlled tests, a good screenshot can be a lucky frame.

## Bugs fixed in this debugging pass

### 1. HeartGold boot/runtime plumbing

Commit:

```text
8d91fc6 Improve HeartGold boot and 3D rendering
```

This was a broad compatibility step, not a single small rendering fix. It
touched direct boot, card/save behavior, interrupts, DMA, GPU2D, GPU3D, audio,
timers, VRAM, and frontend wiring.

Why it mattered:

- HeartGold is a commercial SDK title and exercises many subsystems together.
- A 3D renderer cannot be debugged from a black screen; the system first had
  to reach the point where the game was actually submitting frames.

Important categories included in that commit:

- Slot-1/cart and backup behavior, including HeartGold-specific cart identity.
- Direct boot and boot indicators.
- IRQ/FIFO/DMA paths needed by SDK runtime code.
- GPU2D compositing around the 3D framebuffer.
- Large expansion of the GPU3D command, clipping, texture, raster, and postfx
  paths.

This commit moved the project from "does not usefully reach title graphics" to
"commercial title reaches visible title-scene output."

### 2. W-buffer and fog depth units

Commit:

```text
0cc6372 Align W-buffer and fog depth units
```

Symptom class:

- Depth-dependent effects can be wrong even when geometry appears in roughly
  the right place.
- Fog and W-buffering use hardware depth units, not arbitrary local fixed-point
  units.

Root cause:

- The raster path and post-effect path were using inconsistent depth units.
- That makes fog lookup and W-buffer depth comparisons disagree with each
  other.

Fix:

- Normalize W-buffer conversion and fog depth lookup around the same expanded
  depth-buffer scale.
- Add tests around fog depth units and W-buffer conversion.

Why this matters for HeartGold:

- Title effects and 3D transitions can rely on depth/fog consistency.
- A wrong depth scale can produce geometry that appears to pop, fade, or layer
  incorrectly even if vertex transforms are mostly correct.

### 3. Matrix multiply order false start and correction

Commits:

```text
1fa48d6 Postmultiply NDS matrix transforms
8e72b36 Clarify NDS clip matrix order
4dc396b Restore hardware matrix multiply order
```

Symptom class:

- Wrong matrix order can move, scale, or rotate 3D objects incorrectly.
- This is the kind of bug that can make animations look almost right in some
  frames and badly wrong in others.

Investigation chain:

1. I initially suspected the matrix command composition order was reversed.
2. I changed it toward post-multiply behavior.
3. Re-checking GBATEK showed the command matrix is applied as:

   ```text
   C = M * C
   ```

4. The postmultiply change was therefore wrong and was corrected.

Actual final behavior:

- Matrix commands pre-multiply the current matrix.
- `MTX_SCALE` follows the same command ordering.
- The clip matrix remains position times projection for the row-vector model
  used internally by this codebase.

Why this detail is important:

- This is exactly the kind of trap where copying another emulator or changing
  code to satisfy one screenshot can make the renderer less correct.
- The final state is pinned by hardware docs and regression tests.

### 4. Same-ID translucent double blending

Commit:

```text
8d8ff57 Avoid same-ID translucent double blending
```

Symptom class:

- Translucent polygons can become too bright/dark or visually unstable when
  overlapping fragments from the same polygon ID blend more than once.
- This can show up as flashing, smearing, or noisy translucent layers.

Root cause:

- The rasterizer blended incoming translucent fragments over an existing
  translucent pixel with the same polygon ID.
- NDS polygon IDs are used for more than edge marking; they also affect
  translucent overlap behavior.

Fix:

- Add same-ID rejection for non-shadow translucent fragments.
- Apply it in both filled triangle rasterization and wire/line paths.
- Keep different polygon IDs able to blend, because separate translucent
  objects still need to layer.

Tests added:

```text
test_same_id_translucent_overlap_does_not_blend_twice
test_different_id_translucent_overlap_can_blend_twice
```

Verification:

```sh
cargo test -p nds-core same_id_translucent --release
cargo test -p nds-core gpu3d --release
cargo test -p nds-core --release
cargo build --release -p nds-frontend
```

Result before commit:

- `nds-core` full release suite: passed.
- `nds-frontend` release build: passed.

### 5. Zero-dot polygon alpha behavior

Commit:

```text
433fb7d Honor alpha rules for zero-dot polygons
```

Symptom class:

- Some polygons collapse to a single screen pixel after transform/viewport.
- The old point path treated these as opaque vertex-color writes.
- That bypassed alpha-test, translucent blending, polygon alpha, same-ID
  rejection, and proper depth-update rules.

Root cause:

- Filled triangles and wire lines had accumulated more realistic fragment
  rules, but `draw_point()` still behaved like an early stub:

  ```text
  if depth passes:
      write opaque color
      write depth
      mark edge eligible
  ```

- That is wrong for NDS zero-dot polygons because the polygon's attributes
  still matter even if the projected area collapses.

Fix:

- Compute the final effective alpha from polygon mode and polygon alpha.
- Drop pixels with alpha 0.
- Apply alpha-test when `DISP3DCNT` alpha-test is enabled.
- Apply same-ID translucent rejection.
- Blend translucent pixels when alpha blending is enabled.
- Only update depth for translucent pixels when polygon attr bit 11 allows it.
- Mark edge eligibility only for opaque final pixels.
- Preserve the polygon fog flag.

Tests added:

```text
test_zero_dot_polygon_uses_translucent_alpha
test_zero_dot_polygon_respects_alpha_test
```

Existing zero-dot tests kept passing:

```text
test_zero_dot_polygon_draws_first_vertex_pixel
test_polygon_attr_bit13_keeps_zero_dot_polygon
test_disp_1dot_depth_hides_distant_zero_dot_polygon
```

Verification:

```sh
cargo test -p nds-core zero_dot_polygon --release
cargo test -p nds-core same_id_translucent --release
cargo test -p nds-core alpha_test_requires --release
cargo test -p nds-core gpu3d --release
cargo test -p nds-core --release
cargo build --release -p nds-frontend
```

Result:

- `nds-core` full release suite: `454 passed; 0 failed`.
- `nds-frontend` release build: passed.

Visible effect:

- After this sequence, the latest title screenshot was materially improved and
  recognizable.

### 6. Matrix stack vector preservation guard

Commit:

```text
998a9ac Guard NDS matrix stack vector preservation
```

Symptom class:

- Lighting and normal-vector behavior can break if the position and vector
  matrix stacks desync.
- This can create incorrect shading even when vertex positions are correct.

Investigation:

- GBATEK distinguishes normal matrix updates from stack operations.
- In mode 1, normal matrix arithmetic is not updated by every position matrix
  operation, but position stack operations still preserve the paired vector
  stack state.
- The code already stored/restored both matrices for stack operations, but the
  comment was too vague and there was no explicit regression test.

Fix:

- Clarified the stack comment.
- Added tests proving that mode-1 `PUSH/POP` and `STORE/RESTORE` preserve the
  vector matrix.

Tests added:

```text
test_position_mode_stack_ops_preserve_vector_matrix
test_position_mode_store_restore_preserves_vector_matrix
```

Verification:

```sh
cargo test -p nds-core position_mode --release
cargo test -p nds-core gpu3d::stacks --release
```

Result:

- Stack test suite: `20 passed; 0 failed`.

## What is still missing

The remaining work should not be treated as "just continue translating
melonDS." It needs a test plan.

High-priority areas:

1. **Polygon ordering**
   - Opaque/translucent split.
   - Y sort key behavior.
   - SWAP_BUFFERS bit 0 behavior.
   - Stable ordering when sort keys match.

2. **Raster fill rules**
   - Edge inclusion/exclusion for opaque and translucent polygons.
   - Small polygon behavior.
   - Degenerate line and dot rules.

3. **Clipping**
   - Near/far plane rules.
   - Far-plane bit behavior.
   - Clipped strip vertex reuse.
   - Maximum output vertex counts.

4. **Texture-coordinate transform modes**
   - Mode 1: texture coordinate source.
   - Mode 2: normal source.
   - Mode 3: vertex source.
   - Correct fixed-point scaling and matrix column/row interpretation.

5. **Translucency/shadow corner cases**
   - Same-ID handling is partly covered now.
   - Shadow mask vs visible shadow behavior still needs stronger confirmation.
   - Depth update behavior for translucent fragments needs more ROM-level proof.

6. **Post-effects**
   - Edge marking.
   - Anti-aliasing.
   - Fog alpha-only vs color+alpha.
   - Toon/highlight table behavior.

## Suggested next step

Build or import a small NDS 3D conformance suite before making broad renderer
changes:

- one ROM for polygon ordering,
- one ROM for texture-coordinate transform modes,
- one ROM for clipping/far-plane behavior,
- one ROM for translucent ID behavior,
- one ROM for post-effects.

For HeartGold specifically, capture short frame sequences rather than single
screenshots. A single good frame proves progress; it does not prove the random
polygon flashing is gone.

## 2026-06-02 follow-up fixes before commit

This follow-up batch is intentionally small and test-backed. It contains four
bug groups. Direct reference-emulator implementation use for these four fixes:
**0**. The rules below came from NDS docs / GBATEK plus local behavior tests.

I did previously use melonDS as a sanity check while investigating polygon
ordering behavior, as noted above. That was one investigation thread, not a
source-to-source translation path for this batch.

### 1. `END_VTXS` incorrectly left the vertex list open

Symptom class:

- Geometry commands after `END_VTXS` could continue appending to the previous
  list.
- That makes command streams with explicit begin/end boundaries behave as if
  the end marker was only decorative.

Root cause:

- `VertexState::end()` was a no-op.
- The existing local test even asserted that the active primitive should remain
  active after `END_VTXS`.

Spec basis:

- The NDS vertex command docs describe `BEGIN_VTXS` as starting a vertex list
  and `END_VTXS` as ending that list.
- They also say a new list or swap can implicitly end the current list, but
  that does not make explicit `END_VTXS` a no-op.

Fix:

- `END_VTXS` now calls the same list-closing path used by implicit termination.
- The tests were inverted to prove the list becomes inactive and no unfinished
  polygon remains live after the command.

Why this matters:

- Commercial SDK code may use explicit list boundaries.
- Keeping stale primitive state alive can make later vertices produce unrelated
  triangles or strips, which is one credible source of random polygon flashes.

### 2. Direct GXFIFO writes could satisfy pending packed-command parameters

Symptom class:

- A direct-port geometry write could accidentally complete a packed command
  already waiting for parameters if the opcode matched.
- That mixes two hardware input paths that should be decoded independently.

Root cause:

- The FIFO decoder stored pending direct-port parameters in the same pending
  command queue used by packed command words.
- When a direct `VTX_16` arrived while a packed `VTX_16` was waiting for two
  parameters, the direct parameter could be consumed as if it belonged to the
  packed command.

Spec basis:

- GBATEK describes packed commands as command bytes followed by parameter
  words in the packed FIFO stream.
- Direct command ports are separate command-specific writes: a command with
  `N` parameters is issued by `N` writes to that command port.

Fix:

- Added separate `direct_pending` state for direct-port multi-parameter
  commands.
- Packed pending commands now only consume packed parameter words.
- Direct pending commands now only consume later direct writes to the same
  direct command port.

Test added:

```text
test_direct_port_does_not_satisfy_pending_packed_params
```

Why this matters:

- Once a packed geometry stream is misdecoded, the remaining command stream can
  shift out of phase.
- That kind of bug can make otherwise valid vertex/texture/matrix data appear
  as unrelated command parameters.

### 3. Packed zero-param tail dummy was too broad

Symptom class:

- A packed command word ending in zero padding after a no-parameter command
  incorrectly consumed the next word as a dummy.
- That delayed or dropped the next real command word.

Root cause:

- The decoder treated any packed word whose last decoded command had zero
  parameters as needing a dummy word.
- It did not distinguish a fully occupied four-command packed word from a word
  where the remaining high command bytes were zero padding.

Spec basis:

- GBATEK's packed command examples describe zero bytes as padding / command
  zero.
- The no-parameter "overkill" dummy case applies when a real zero-parameter
  command occupies the final command slot of a full packed word, not when the
  rest of the word is zero padding.

Fix:

- Track how many non-zero command slots were used in the packed word.
- Require the tail dummy only when all four slots were occupied and the fourth
  command had zero parameters.

Tests updated/added:

```text
test_zero_padded_packed_word_ending_with_zero_param_command_needs_no_dummy
test_full_packed_word_ending_with_zero_param_command_requires_dummy_word
test_packed_word_tail_dummy_waits_until_prior_params_consumed
```

Why this matters:

- Incorrect dummy consumption desynchronizes the GXFIFO stream.
- For real game command streams, one consumed command word can corrupt many
  following polygons.

### 4. 3D-as-BG0 compositing ignored hardware-facing details

Symptom class:

- When the 3D renderer is mapped into Engine A BG0, the compositor treated it
  too much like an ordinary opaque 2D BG layer.
- Per-pixel 3D alpha was not carried into 2D blending.
- BG0 horizontal scroll did not affect the 3D BG0 source.

Root cause:

- `BgPixel` had no place to carry 3D alpha.
- The compositor always used `BLDALPHA` for alpha blending instead of the 3D
  pixel alpha when the top target was 3D BG0.
- The 3D framebuffer synthesis path sampled `x` directly and ignored BG0
  horizontal offset.

Spec basis:

- The NDS display path exposes 3D output through Engine A BG0.
- 3D pixels carry their own alpha, while 2D alpha blending uses the 2D blend
  control path.
- BG0 scroll registers still participate in the Engine A BG source path, so
  the 3D source must be sampled through that BG0 coordinate path.

Fix:

- Added optional per-pixel `alpha_3d` to BG pixels.
- Threaded the 3D rasterizer alpha buffer into Engine A scanline rendering.
- Used 3D alpha for 3D BG0 first-target blending when a valid second target is
  present.
- Applied BG0 horizontal scroll to the synthesized 3D BG0 layer, with the
  256..511 half treated as transparent.

Tests added:

```text
test_3d_bg0_first_target_uses_3d_alpha_not_bldalpha
test_3d_bg0_horizontal_scroll_exposes_transparent_half
```

Why this matters:

- HeartGold uses 2D and 3D together on the title scene.
- If 3D alpha or BG0 source coordinates are wrong, the game can boot and still
  have wrong layering, effects, or displaced title elements.

## Verification for this follow-up batch

Commands run:

```sh
cargo fmt
cargo test -p nds-core gpu3d::fifo --release
cargo test -p nds-core --release
cargo build --release -p nds-frontend
```

Result:

- `nds-core` release tests: `473 passed; 0 failed`.
- `nds-frontend` release build: passed.

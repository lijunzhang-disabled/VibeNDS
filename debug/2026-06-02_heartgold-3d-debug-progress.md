# HeartGold 3D debug progress

Date: 2026-06-02
Status: **In progress**

## Current status

HeartGold is no longer a black-screen boot failure. It reaches a recognizable
title scene, and the latest Desktop screenshot showed the Ho-Oh title art and
`TOUCH TO START` instead of a blank frame or broken layout.

Latest direct capture from the rebuilt release binary:

```sh
./target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 4200 --capture-ppm /private/tmp/heartgold-current.ppm
```

Result:

- Capture completed successfully at `256 x 384`.
- The default screen gap is no longer present.
- The title frame is coherent: Ho-Oh scene on the top screen, HeartGold logo on
  the bottom screen, and no random full-frame polygon corruption in that frame.
- Remaining visible risk is now finer 3D raster/post-effect correctness in the
  top-screen scene, rather than the earlier black-screen, giant-gap, or random
  polygon state.

Short sequence check:

```sh
./target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 4320 --capture-dir /private/tmp/heartgold-seq --capture-interval 60
```

Result:

- Sequence capture completed through frame 4320.
- Late sampled frames `4200`, `4260`, and `4320` were generated for inspection.
- Frame `4320` remained coherent and did not reproduce the previously reported
  random-polygon flashing.

Current 5400-frame sequence check:

```sh
./target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 5400 --capture-interval 540 --capture-dir /private/tmp/heartgold-20260606-current-verify
```

Result:

- Sequence capture completed through frame 5400.
- Captures were `256 x 384`, matching the compact dual-screen layout.
- Frame `540` showed the expected Game Freak splash.
- Frame `2700` showed coherent title-animation art.
- Frames `4320` and `5400` showed stable Ho-Oh title frames with the bottom
  HeartGold logo and `TOUCH TO START`.
- The sampled sequence did not reproduce the earlier black screen, oversized
  vertical gap, or random polygon flashing failure modes.

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

Direct reference-implementation audit for today's committed fixes:

- Bugs fixed by directly translating or copying a reference implementation:
  **0**.
- Bugs where a reference emulator was used only as a sanity check during
  investigation: **1 area**, polygon ordering.
- Bugs fixed from NDS docs / GBATEK plus local regression tests: the FIFO
  command-stream fixes, GXSTAT/status fixes, viewport handling, `VEC_TEST`
  matrix-mode behavior, `END_VTXS` no-op behavior, and `BEGIN_VTXS` restart
  coverage.

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

### 1. Superseded: `END_VTXS` vertex-list handling

Status: **Superseded by the later `END_VTXS` no-op correction below.**

The analysis in this subsection preserved an early interpretation that treated
`END_VTXS` as an explicit list terminator. That matched a plain reading of
the command name, but it did not match the later hardware check against
GBATEK/no$gba behavior.

Earlier assumed symptom class:

- Geometry commands after `END_VTXS` could continue appending to the previous
  list.
- That makes command streams with explicit begin/end boundaries behave as if
  the end marker was only decorative.

Why that was wrong:

- Real hardware treats `END_VTXS` as decorative; the local no-op behavior was
  directionally correct.
- The actual bug was the later change that made `END_VTXS` clear active
  primitive state.

Misread spec basis:

- The NDS vertex command docs describe `BEGIN_VTXS` as starting a vertex list
  and `END_VTXS` as ending that list.
- They also say a new list or swap can implicitly end the current list, but
  this was not enough evidence to override GBATEK's explicit no-op note.

Superseded attempted fix:

- `END_VTXS` was temporarily changed to call the same list-closing path used
  by implicit termination.
- That behavior has now been reversed by the later no-op correction.

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

Status: **Superseded later in this document**. This was an intermediate
interpretation from the first FIFO pass. The later "packed zero-param tail
dummy removed" section is the current implementation and test-backed result.

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

## 2026-06-02 status/FIFO/viewport follow-up

This batch contains three emulator behavior fixes and two doc corrections.
Direct reference-emulator implementation use for this batch: **0**. The
changes were driven by NDS docs / GBATEK register behavior and local regression
tests.

### 1. FIFO overflow was incorrectly exposed as `GXSTAT[15]`

Symptom class:

- The emulator treated the FIFO's internal overflow flag as if it were a
  hardware-visible `GXSTAT` low-bit condition.
- Writing `GXSTAT[15]` cleared both matrix-stack overflow and the FIFO
  overflow flag.

Root cause:

- `GXSTAT[15]` is the matrix stack overflow/underflow flag.
- It is not a FIFO overflow bit and should not reflect command FIFO state.

Spec basis:

- NDS 3D status docs list `GXSTAT[15]` as matrix stack overflow/underflow.
- FIFO count and status live in the high status bits: count at
  `GXSTAT[16..24]`, less-than-half at `GXSTAT[25]`, empty at `GXSTAT[26]`,
  and general busy at `GXSTAT[27]`.

Fix:

- `Engine3d::gxstat_low()` now reports bit 15 only from the matrix stack
  overflow flag.
- `Engine3d::write_gxstat()` now clears only the matrix stack overflow flag
  when software writes bit 15.
- Removed the stale FIFO low-status helper that encoded non-hardware low bits.

Tests added/updated:

```text
test_gxstat_write_clears_stack_error_and_sets_irq_mode
test_fifo_overflow_does_not_set_gxstat_matrix_stack_error
```

Why this matters:

- Games poll `GXSTAT` for synchronization and diagnostics.
- Reporting a fake matrix-stack error from FIFO state can send SDK code down
  the wrong recovery path or mask the real stack-error condition.

### 2. Full GXFIFO writes dropped command data instead of stalling/preserving

Symptom class:

- When the emulated FIFO reached 256 entries, later command writes were
  dropped and an overflow flag was set.
- That can corrupt a valid geometry command stream.

Root cause:

- Real hardware stalls CPU writes to GXFIFO/geometry command ports while the
  FIFO is full.
- The emulator does not yet model the exact CPU stall timing, but dropping
  command words is less accurate than preserving order.

Spec basis:

- GBATEK describes CPU writes to the geometry FIFO as waiting when the FIFO is
  full.
- The hardware-visible FIFO count is capped at 256 entries.

Fix:

- Packed and direct FIFO writes now preserve over-capacity command data in
  order.
- `GXSTAT` count reporting remains capped to the 256-entry hardware-visible
  maximum.
- The internal overflow flag is no longer used to model full-FIFO writes.

Tests updated:

```text
test_packed_command_word_past_capacity_preserves_commands
test_direct_port_write_past_full_preserves_command_stream
```

Why this matters:

- A single dropped command or parameter can shift the stream and turn later
  vertex data into unrelated commands.
- That failure mode matches random polygon flashes much more closely than a
  clean, deterministic transform error.

### 3. VIEWPORT used exclusive span instead of inclusive hardware size

Symptom class:

- Full-screen viewport center mapped to `(127.5, 95.5)` instead of
  `(128, 96)`.
- Right/bottom NDC edges mapped to `X2/Y1` rather than the one-pixel-beyond
  hardware mathematical edge.

Root cause:

- The transform scaled by `x2 - x1` and `y2 - y1`.
- NDS viewport math uses the inclusive size:

  ```text
  width  = x2 - x1 + 1
  height = y2 - y1 + 1
  ```

Spec basis:

- The NDS viewport docs note that polygons can render one pixel beyond
  `(X2, Y1)`.
- The viewport Y coordinates are lower-left-origin values, while the emulator
  framebuffer is top-left-origin.

Fix:

- Screen X now uses `x1 + (ndc_x + 1) * width / 2`.
- Screen Y now converts from lower-left viewport coordinates with
  `top_y = 191 - y2`, then applies `(1 - ndc_y) * height / 2`.
- Full-screen right/bottom mathematical edges can land at `256/192`, with the
  raster output still clipped by the physical framebuffer.

Tests updated/added:

```text
test_perspective_divide_centers_at_screen_center
test_perspective_divide_right_edge
test_viewport_edges_extend_one_pixel_beyond_x2_y1
test_partial_viewport_y_uses_lower_left_origin
```

Why this matters:

- Viewport scale is applied to every transformed 3D vertex.
- A half-pixel/full-pixel mismatch can affect edge placement, tiny polygons,
  and title-scene composition even when matrices are otherwise correct.

## Verification for status/FIFO/viewport follow-up

Commands run:

```sh
cargo test -p nds-core gpu3d::fifo --release
cargo test -p nds-core gxstat --release
cargo test -p nds-core gpu3d::viewport --release
cargo test -p nds-core --release
```

Result:

- `gpu3d::fifo` targeted tests: passed.
- `gxstat` targeted tests: passed.
- `gpu3d::viewport` targeted tests: passed.
- `nds-core` full release suite: `475 passed; 0 failed`.

## 2026-06-02 correction: packed zero-param tail dummy

Status: **Superseded by the following correction**

Direct reference-emulator implementation use for this correction: **0**. The
trigger was re-reading the ndsdoc 3D Geometry Engine FIFO text.

### What was wrong in the previous note

The earlier follow-up section said the packed zero-parameter tail dummy was
"too broad" and only required the dummy when all four command slots were
occupied. That interpretation was too narrow.

ndsdoc states that when using the FIFO directly, if the last command in a
command word has no parameter, software must write `0` as a dummy parameter
before the hardware accepts a new command word. The same paragraph also says
invalid command indices behave like command index `0`, and a later note says
zero command indices may only appear as trailing padding.

The important correction is:

- zero bytes still terminate/pad the command word;
- but the "last command" means the last real non-zero command before that
  padding;
- if that real command has zero parameters, the next FIFO word is still a
  dummy.

### Symptom class

- A command stream like `MTX_PUSH` followed immediately by `MTX_MODE` through
  the packed FIFO was decoded as two command words.
- Hardware expects the second word to be consumed as the dummy for
  `MTX_PUSH`; the real `MTX_MODE` command word comes after that.

### Root cause

- `needs_zero_param_tail_dummy` was set only when all four packed command slots
  were occupied and the fourth command had zero parameters.
- That ignored the zero-padded case where the first, second, or third command
  slot was the final real command.

### Fix

- Set `needs_zero_param_tail_dummy` whenever a packed command word contains at
  least one real command and the last real command has zero parameters.
- Keep the existing behavior that command index `0` terminates the command
  list and later non-zero bytes in that same word are ignored.

Tests updated:

```text
test_zero_padded_packed_word_ending_with_zero_param_command_requires_dummy
test_packed_word_tail_dummy_waits_until_prior_params_consumed
test_full_packed_word_ending_with_zero_param_command_requires_dummy_word
```

Why this matters:

- Packed GXFIFO command streams are commonly DMA-fed.
- Missing a dummy word shifts the command/parameter boundary and can turn a
  valid stream into visible random geometry.

## 2026-06-02 correction: packed zero-param tail dummy removed

Status: **Corrected again after checking GBATEK's FIFO overkill note**

Direct reference-emulator implementation use for this correction: **0**. This
comes from re-reading GBATEK's `GXFIFO / Packed Commands` and `GXFIFO DMA
Overkill on Packed Commands Without Parameters` sections.

### What was wrong in the previous correction

The previous section said that a packed command word whose final real command
has no parameters must consume the next FIFO word as a dummy. That does not
match GBATEK's overkill example:

```text
Packed(00151515h)
```

GBATEK describes repeated words of that form as producing many `Cmd(15h)`
entries. If a dummy word were required after each such packed word, every other
word would be discarded instead of producing commands, and the documented
overfill behavior would not happen.

### Fix

- Removed `needs_zero_param_tail_dummy` from the packed FIFO decoder.
- Zero-parameter commands still occupy FIFO entries, but they do not consume
  parameter words.
- A following FIFO word is decoded as the next packed command word once all
  pending parameters have been satisfied.

Tests updated/added:

```text
test_zero_padded_packed_word_ending_with_zero_param_command_allows_next_command_word
test_zero_param_tail_after_prior_params_allows_next_command_word
test_full_packed_word_ending_with_zero_param_command_allows_next_command_word
test_repeated_packed_identity_words_do_not_need_dummy_words
```

Why this matters:

- DMA-fed command streams can legitimately contain many packed no-parameter
  commands.
- Treating following words as dummy data drops commands and shifts the stream,
  which can produce missing transforms or random-looking geometry.

## 2026-06-02 correction: `VEC_TEST` matrix mode

Status: **Fixed**

Direct reference-emulator implementation use for this correction: **0**. This
came from comparing the local test-command handler with ndsdoc and GBATEK.

### Symptom class

- `VEC_TEST` produced a result even when the active matrix mode was not
  position+vector mode.
- That let software read a plausible result from the vector-test registers
  after issuing the command in a mode where hardware documentation says the
  command is not valid.

### Root cause

- `handle_vec_test()` always multiplied the input vector by
  `self.stacks.vector`.
- The current `MTX_MODE` was ignored.

### Spec basis

- ndsdoc says `VEC_TEST` multiplies `(x,y,z,0)` by the directional matrix
  stack and notes the matrix-mode-2 requirement.
- GBATEK states the same rule more directly: `VEC_TEST`, like `NORMAL`,
  requires Matrix Mode 2, the Position & Vector Simultaneous Set mode.

### Fix

- `handle_vec_test()` now returns without updating `VEC_RESULT` unless
  `self.stacks.mode == MtxMode::PosVector`.
- The top-level bus test now selects `MTX_MODE = 2` before issuing
  `VEC_TEST`.

Tests added/updated:

```text
test_vec_test_requires_pos_vector_matrix_mode
test_vec_test_readback_wraps_overflowed_unit_vector
test_vec_test_writes_direction_result_registers
```

Why this matters:

- `VEC_TEST` is part of the documented geometry-test path used by software for
  visibility and vector-space checks.
- Letting it work in the wrong mode hides command-stream or matrix-mode bugs
  that real hardware would expose.

## 2026-06-02 correction: `END_VTXS` is a hardware no-op

Status: **Fixed after reversing the earlier local interpretation**

Direct reference-emulator implementation use for this correction: **0**. This
came from comparing the local vertex-list state machine with ndsdoc's
"optional" end-command framing and GBATEK's explicit note that `END_VTXS` has
no effect on real NDS/no$gba.

### Symptom class

- `END_VTXS` cleared the active primitive and vertex buffer.
- A command stream that emitted `BEGIN_VTXS`, one or two vertices,
  `END_VTXS`, then more vertices would lose the later vertices locally.
- An incomplete list followed by `END_VTXS` and `SWAP_BUFFERS` could avoid the
  geometry lock-up path, even though hardware still considers the list state
  live.

### Root cause

- `VertexState::end()` called `force_end()`.
- That made the explicit `END_VTXS` command behave like the implicit
  termination performed by `BEGIN_VTXS` and `SWAP_BUFFERS`.

### Spec basis

- ndsdoc says `END_VTXS` is optional because vertex lists are automatically
  ended when a new one begins or when buffers are swapped.
- GBATEK is more direct: `END_VTXS` has no effect on real hardware and may be
  issued multiple times inside a vertex list.

### Fix

- `VertexState::end()` is now a no-op.
- `force_end()` remains the internal path for events that really terminate a
  list: `BEGIN_VTXS` clears and restarts state, and `SWAP_BUFFERS` force-ends
  after accepting a complete list.

Tests added/updated:

```text
test_end_vtxs_is_noop_inside_active_list
test_end_vtxs_command_is_noop_for_vertex_submission
test_end_vtxs_does_not_hide_incomplete_list_from_swap_lock
test_end_vtxs_direct_port_is_noop_via_arm9_io
test_begin_vtxs_restarts_list_and_discards_partial_vertices
test_begin_vtxs_direct_port_restarts_partial_list_via_arm9_io
```

Why this matters:

- The emulator should not discard vertices simply because software emits the
  decorative end marker.
- Keeping incomplete-list state visible to `SWAP_BUFFERS` preserves the
  documented lock-up behavior instead of hiding malformed command streams.

## 2026-06-02 conformance coverage: texture-coordinate transform bottom row

Status: **Coverage added; no implementation change required**

Direct reference-emulator implementation use for this check: **0**. This came
from GBATEK's texture-coordinate transform formulas.

### Rule checked

In texture coordinate transform modes 2 and 3, the texture matrix still exists
as a 4x4 matrix, but the bottom row used by the formula is replaced by the most
recent `TEXCOORD` command's `S` and `T` values:

- Mode 2 (`Normal source`) evaluates on `NORMAL`.
- Mode 3 (`Vertex source`) evaluates on each `VTX_*` command.
- `m[12]` and `m[13]` from the texture matrix must not add an extra
  translation in those modes.

### Local result

The current implementation already matched that rule: modes 2 and 3 add the
raw `TEXCOORD` base values and ignore the texture matrix bottom-row
translation. I added regression tests to lock this down because texture
coordinate transforms are a plausible source of title-scene texture corruption.

Tests added:

```text
test_texcoord_transform_mode_2_replaces_matrix_bottom_row_with_texcoord
test_texcoord_transform_mode_3_replaces_matrix_bottom_row_with_texcoord
```

Verification:

```sh
cargo test -p nds-core gpu3d::vertex --release
cargo test -p nds-core --release
```

Result:

- `gpu3d::vertex` release tests: `24 passed; 0 failed`.
- `nds-core` full release suite: `483 passed; 0 failed`.

## 2026-06-02 correction: fog alpha first-boundary hardware glitch

Status: **Fixed**

Direct reference-emulator implementation use for this correction: **0**. This
came from GBATEK's fog post-effect notes.

### Symptom class

- Fog alpha was blended with `FOG_COLOR` alpha for every fogged pixel.
- On real hardware, the fog alpha value is treated as `31` in the region up to
  the first fog depth boundary.
- With nonzero `FOG_TABLE[0]` and a low fog alpha, local output could make
  near fogged pixels incorrectly translucent or fully transparent.

### Root cause

- `apply_fog()` used `FOG_COLOR[20:16]` directly for alpha blending
  regardless of depth region.
- The density logic already understood the first boundary, but the alpha path
  did not account for the documented hardware quirk.

### Spec basis

- GBATEK notes that fog alpha appears to be ignored, effectively treated as
  `31`, up to the first density boundary.
- The note also explains why this is often invisible in games: density 0 is
  commonly zero, so multiplying by that region's density hides the quirk.

### Fix

- Added `fog_alpha_glitch_uses_full_alpha()` to detect the first-boundary
  region in the same depth units used by fog density.
- `apply_fog()` now substitutes fog alpha `31` only for that region and keeps
  normal `FOG_COLOR` alpha elsewhere.

Tests added:

```text
test_fog_alpha_uses_full_alpha_before_first_boundary
```

Verification:

```sh
cargo test -p nds-core gpu3d::raster::postfx --release
cargo test -p nds-core --release
```

Result:

- `gpu3d::raster::postfx` release tests: `17 passed; 0 failed`.
- `nds-core` full release suite: `484 passed; 0 failed`.

## 2026-06-02 correction: shadow mode on degenerate line/point paths

Status: **Fixed**

Direct reference-emulator implementation use for this correction: **0**. This
came from GBATEK's shadow polygon behavior plus local raster-path inspection.

### Symptom class

- Normal filled shadow polygons used the shadow stencil rules.
- Degenerate polygons that rasterized as line segments or points bypassed
  those rules and could write color directly.
- That made shadow mask lines behave like visible translucent geometry instead
  of stencil-only mask geometry.

### Root cause

- `rasterize_scanline()` had explicit shadow handling for normal triangle
  pixels.
- `draw_wire_line()` and `draw_point()` had their own fragment write paths and
  only skipped same-ID translucent rejection for shadow mode; they did not
  apply the shadow mask / visible-shadow stencil rules.

### Spec basis

- GBATEK describes shadow mode as a two-step stencil process:
  - Polygon ID `0` writes the shadow mask and does not write the color buffer.
  - Polygon ID `1..3Fh` draws only where the stencil is clear and where the
    destination polygon ID differs; if a stencil bit is set, it is cleared and
    the visible shadow pixel is skipped.
- The polygon definition docs also say line segments are represented by
  degenerate triangles, so shadow-mode degenerate triangles should not escape
  shadow-mode semantics.

### Fix

- Added `shadow_fragment_is_hidden_or_masked()` as the shared shadow fragment
  decision.
- Routed normal triangles, degenerate line segments, and zero-dot point
  fragments through that helper before color/depth/fog/id writes.

Tests added:

```text
test_shadow_mask_line_does_not_write_color
test_visible_shadow_line_draws_only_where_mask_is_clear
```

Verification:

```sh
cargo test -p nds-core shadow --release
cargo test -p nds-core --release
```

Result:

- Shadow-targeted release tests: `5 passed; 0 failed`.
- `nds-core` full release suite: `486 passed; 0 failed`.

## 2026-06-02 correction: decoded FIFO ops keep geometry busy

Status: **Fixed**

Direct reference-emulator implementation use for this correction: **0**. This
came from ndsdoc's 3D command FIFO and readable-matrix descriptions plus local
FIFO-state inspection.

### Symptom class

- A packed FIFO command word can contain several zero-parameter geometry
  commands.
- After the first decoded command was popped, the raw packed word could already
  be removed from the FIFO word queue while the remaining decoded commands were
  still waiting in the dispatch queue.
- `Engine3d::geometry_busy()` used `fifo.is_empty()`, and `fifo.is_empty()`
  only checked the raw word queue.
- That could make `GXSTAT.27` report idle too early and make readable matrices
  return real matrix values while decoded geometry commands were still pending.

### Root cause

- `GxFifo` has multiple internal queues/states:
  - raw FIFO words,
  - pending packed commands waiting for parameters,
  - pending direct-port commands waiting for parameters,
  - decoded ready commands waiting for the geometry dispatcher.
- Empty status for geometry-busy purposes must mean all of those are empty,
  not only that no raw words remain.
- The bug is easiest to trigger with a packed command word such as
  `0x1515_1515`, which decodes to four `MTX_IDENTITY` commands. After popping
  one command, three decoded ready commands remain even though the shared raw
  command word has been consumed.

### Spec basis

- ndsdoc describes direct FIFO use as a command word containing up to four
  packed command indices, followed by parameters in command order.
- ndsdoc also says readable matrices require the geometry engine to be
  disabled via `GXSTAT.27`.
- Therefore decoded-but-not-dispatched commands are still geometry-engine work
  and must keep the busy bit active.

### Fix

- Changed `GxFifo::is_empty()` to require every internal FIFO/decode state to
  be empty:
  - `entries == 0`,
  - raw words empty,
  - no pending packed command,
  - no pending direct command,
  - no decoded ready command.
- Left entry counting and command decoding unchanged.

Tests added:

```text
test_ready_ops_keep_fifo_nonempty_after_shared_packed_word_is_spent
test_decoded_ready_fifo_ops_keep_geometry_busy
```

Verification:

```sh
cargo test -p nds-core fifo --release
cargo test -p nds-core test_decoded_ready_fifo_ops_keep_geometry_busy --release
```

Result:

- FIFO-targeted release tests: `33 passed; 0 failed`.
- Engine ready-FIFO busy regression: `1 passed; 0 failed`.

## 2026-06-02 correction: VEC_TEST result 32-bit I/O readback

Status: **Fixed**

Direct reference-emulator implementation use for this correction: **0**. This
came from ndsdoc's `VEC_TEST` result-register description plus local ARM9 I/O
readback inspection.

### Symptom class

- `VEC_TEST` writes three signed 4.12 halfword results at
  `0x04000630..=0x04000635`.
- The ARM9 I/O path supported 16-bit reads from that region.
- The 32-bit read path did not handle `0x04000630` or `0x04000634`, so word
  reads returned the default unmapped value `0`.
- Software that reads the test result as words would see an all-zero direction
  result even though halfword reads were correct.

### Root cause

- `read_io16()` had an explicit `0x0630..=0x0634` case.
- `read_io32()` handled `POS_TEST`, clip matrix, and directional matrix
  readback, but skipped the `VEC_TEST` result region.

### Spec basis

- ndsdoc describes the direction-test result at `0x04000630..=0x04000635`,
  with each 2-byte halfword corresponding to one transformed vector
  coordinate.
- Since the DS I/O bus exposes this region as normal memory-mapped registers,
  a 32-bit read at `0x04000630` should return the X and Y halfwords packed in
  little-endian order, and a 32-bit read at `0x04000634` should return Z in
  the low halfword.

### Fix

- Added `read_io32()` cases for:
  - `0x0630`: packed X/Y direction-test halfwords,
  - `0x0634`: Z direction-test halfword in the low halfword.
- Left the existing `VEC_TEST` math and 16-bit readback unchanged.

Tests added:

```text
test_vec_test_result_registers_support_word_reads
```

Verification:

```sh
cargo test -p nds-core test_vec_test_result_registers_support_word_reads --release
cargo test -p nds-core test_vec_test_writes_direction_result_registers --release
cargo test -p nds-core --release
```

Result:

- VEC_TEST word-read regression: `1 passed; 0 failed`.
- Existing VEC_TEST halfword regression: `1 passed; 0 failed`.
- `nds-core` full release suite: `489 passed; 0 failed`.

## 2026-06-02 conformance coverage: strip completeness before SWAP_BUFFERS

Status: **Coverage added; no implementation change required**

Direct reference-emulator implementation use for this check: **0**. This came
from ndsdoc's vertex-list primitive counts and the SWAP/vertex-list behavior.

### Rule checked

- `BEGIN_VTXS` primitive type 2, triangle strip, completes the first polygon
  at 3 vertices and each additional vertex completes another triangle.
- `BEGIN_VTXS` primitive type 3, quad strip, completes the first polygon at
  4 vertices and each additional pair of vertices completes another quad.
- `SWAP_BUFFERS` with a genuinely incomplete list should lock the geometry
  engine, so the helper that detects incomplete lists must distinguish complete
  strip states from partial strip tails.

### Local result

The current implementation already matched that rule:

- Triangle strip lengths 1 and 2 are incomplete; lengths 3 and higher are
  complete.
- Quad strip lengths 1, 2, and 3 are incomplete; length 4 is complete; odd
  tails after that are incomplete until the next vertex pair arrives.

I added a regression test because this is the exact predicate that decides
whether `SWAP_BUFFERS` succeeds or locks the geometry engine.

Tests added:

```text
test_strip_incomplete_list_detection_matches_primitive_vertex_counts
```

Verification:

```sh
cargo test -p nds-core gpu3d::vertex --release
cargo test -p nds-core --release
```

Result:

- Vertex targeted release tests: `25 passed; 0 failed`.
- `nds-core` full release suite: `490 passed; 0 failed`.

## 2026-06-02 conformance coverage: lighting/material field decoding

Status: **Coverage added; no implementation change required**

Direct reference-emulator implementation use for this check: **0**. This came
from ndsdoc and GBATEK light/material command field definitions.

### Rule checked

- `SPE_EMI` low 15 bits set specular reflection color.
- `SPE_EMI` bit 15 enables the shininess table.
- `SPE_EMI` bits 16-30 set emission color.
- `LIGHT_COLOR` low 15 bits set BGR555 light color.
- `LIGHT_COLOR` bits 30-31 select light index `0..3`.

### Local result

The current implementation already matched those command field definitions. I
added direct unit coverage because these fields affect every following
`NORMAL` color calculation and can be hard to isolate once transformed into
final vertex colors.

Tests added:

```text
test_set_spe_emi_unpacks_correctly
test_light_color_unpacks_index_and_color
```

Verification:

```sh
cargo test -p nds-core gpu3d::lighting --release
cargo test -p nds-core --release
```

Result:

- Lighting targeted release tests: `10 passed; 0 failed`.
- `nds-core` full release suite: `492 passed; 0 failed`.

## 2026-06-02 conformance coverage: mode-1 matrix stack special case

Status: **Coverage added; no implementation change required**

Direct reference-emulator implementation use for this check: **0**. This came
from ndsdoc matrix-stack descriptions and GBATEK's note that stack operations
in matrix mode 1 act like mode 2.

### Rule checked

- `MTX_MODE=1` normally selects the position matrix for load/multiply/scale
  and translate operations.
- `MTX_PUSH`, `MTX_POP`, `MTX_STORE`, and `MTX_RESTORE` are a documented
  special case: in mode 1 they operate on both the position and directional
  stacks, same as mode 2.
- The shared position/directional stack pointer must therefore restore both
  matrices on a mode-1 pop/restore.

### Local result

The implementation already matched the special case. I renamed two misleading
tests so they describe save/restore behavior instead of implying the vector
matrix is ignored, and added direct coverage that mode-1 push/pop restores
both position and vector matrices.

Tests added/adjusted:

```text
test_position_mode_stack_ops_touch_both_position_and_vector_stacks
test_position_mode_stack_ops_save_and_restore_vector_matrix
test_position_mode_store_restore_save_and_restore_vector_matrix
```

Verification:

```sh
cargo test -p nds-core gpu3d::stacks --release
cargo test -p nds-core --release
```

Result:

- Stack targeted release tests: `21 passed; 0 failed`.
- `nds-core` full release suite: `493 passed; 0 failed`.

## 2026-06-02 correction: zero-dot wireframe polygons keep fixed edge alpha

Status: **Fixed**

Direct reference-emulator implementation use for this correction: **0**. This
came from GBATEK's `POLYGON_ATTR` alpha description plus local raster path
inspection.

### Symptom class

- `POLYGON_ATTR.alpha=0` selects wireframe rendering.
- The normal wireframe line path already treated those edge pixels as fixed
  alpha 31, matching the hardware rule.
- A degenerate polygon whose vertices all collapse to the same screen pixel
  takes the zero-dot `draw_point()` path instead of the wire-line path.
- That point path read `POLYGON_ATTR.alpha=0` as an effective transparent
  alpha and returned without writing the pixel.

### Root cause

- `draw_wire_line()` had the alpha-0 wireframe special case:
  `attr alpha 0 -> edge alpha 31`.
- `draw_point()` did not share that special case, even though zero-dot
  polygons are another degenerate edge-only case in the same raster branch.
- The result was inconsistent raster behavior:
  - degenerate line with alpha 0 rendered as an opaque wire edge,
  - degenerate point with alpha 0 disappeared.

### Spec basis

- GBATEK documents `POLYGON_ATTR` alpha as:
  - `0 = Wire-Frame`,
  - `1..30 = Translucent`,
  - `31 = Solid`.
- It also states that the interior of wireframe polygons is transparent and
  only polygon edge lines are rendered, using fixed alpha 31.
- A zero-dot degenerate polygon has no interior to fill, so the visible
  edge-only fragment should use the same fixed wireframe alpha as degenerate
  lines.

### Fix

- Changed `draw_point()` to translate `POLYGON_ATTR.alpha=0` to effective
  polygon alpha 31 before calling `final_alpha()`, matching `draw_wire_line()`.
- Left normal translucent zero-dot behavior unchanged for alpha `1..30`.
- Left alpha-test/depth/shadow/translucent-ID behavior unchanged after the
  corrected effective alpha is computed.

Tests added:

```text
test_zero_dot_wireframe_polygon_uses_fixed_opaque_alpha
```

Verification:

```sh
cargo test -p nds-core gpu3d::raster::triangle --release
cargo test -p nds-core --release
```

Result:

- Raster triangle targeted release tests: `43 passed; 0 failed`.
- `nds-core` full release suite: `494 passed; 0 failed`.

## 2026-06-02 conformance coverage: POS_TEST seeds inherited vertex coordinates

Status: **Coverage added; no implementation change required**

Direct reference-emulator implementation use for this check: **0**. This came
from ndsdoc's `POS_TEST` note plus local ARM9 I/O behavior inspection.

### Rule checked

- `POS_TEST` multiplies `(x, y, z, 1)` by the position/projection path and
  writes the result registers at `0x04000620..=0x0400062F`.
- ndsdoc also notes that the command updates the coordinate state inherited by
  partial vertex-position commands.
- Therefore a later `VTX_XY` should use the Z coordinate supplied by the most
  recent `POS_TEST`, even though `POS_TEST` itself does not submit a polygon
  vertex.

### Local result

The current implementation already matched the documented side effect:

- `handle_pos_test()` decodes the same 16-bit position format as `VTX_16`,
- stores the decoded position in `vertex.last_pos`,
- writes only the test-result registers and does not submit a vertex.

I added direct ARM9 I/O coverage because this behavior is easy to remove by
mistake when treating `POS_TEST` as a pure readback-only command.

Tests added:

```text
test_pos_test_seeds_inherited_vertex_position_components
```

Verification:

```sh
cargo test -p nds-core test_pos_test --release
cargo test -p nds-core --release
```

Result:

- POS_TEST targeted release tests: `2 passed; 0 failed`.
- `nds-core` full release suite: `495 passed; 0 failed`.

## 2026-06-02 fix: alpha=0 wireframe translucent texture render order

Status: **Fixed**

Direct reference-emulator implementation use for this fix: **0**. This came
from GBATEK display-control, polygon-attribute, and texture-attribute rules
plus local raster pipeline inspection.

### Symptom

HeartGold's title scene was still visually unstable after the earlier geometry
fixes. One local mismatch found during the raster audit was that wireframe
polygons with translucent texture formats could be classified into the opaque
render pass even though their final edge fragments can be translucent.

The concrete bad case:

- `POLYGON_ATTR.alpha = 0` marks a polygon as wireframe.
- Wireframe edge fragments use fixed polygon alpha 31.
- A3I5/A5I3 texture formats carry per-texel alpha.
- In modulation and toon/highlight texture modes, the texture alpha contributes
  to the final fragment alpha.
- If such a polygon was submitted before opaque geometry, the old pass
  classifier drew it in the opaque pass, so a later opaque polygon could
  overwrite it instead of letting the wireframe edge draw/blend in the late
  translucent pass.

### Root cause

`is_translucent()` returned `false` immediately when
`POLYGON_ATTR.alpha == 0`. That was correct for plain wireframe edges because
their fixed effective edge alpha is 31, but it ignored texture alpha. The
lower-level wireframe raster path already used texture alpha for A3I5/A5I3
edge fragments, so render-order classification and pixel shading disagreed.

### Spec basis

- GBATEK's polygon attribute table defines alpha 0 as wireframe and notes that
  wireframe edge lines use fixed alpha 31.
- GBATEK's display-control notes describe alpha testing/blending as applying
  to final polygon pixels after texture blending.
- GBATEK's texture-attribute table marks A3I5 and A5I3 as translucent texture
  formats.

The combination means alpha-0 wireframes are not automatically translucent,
but alpha-0 wireframes using translucent texture alpha in modulation or
toon/highlight mode can produce final alpha below 31 and therefore belong in
the translucent render pass.

Decal mode remains different: decal texture alpha controls color mixing while
the final fragment alpha remains the polygon alpha. For alpha-0 wireframe
edges, that effective polygon alpha is 31, so decal wireframes remain opaque
for render-order purposes.

### Fix

- Removed the early `alpha == 0 => opaque` return from `is_translucent()`.
- Kept `alpha 1..30` as translucent.
- Applied the existing A3I5/A5I3 texture-alpha classification to both
  `alpha == 31` solid polygons and `alpha == 0` wireframe polygons in
  modulation/toon modes.
- Kept A3I5/A5I3 decal wireframes in the opaque pass.

Tests added:

```text
test_wireframe_translucent_texture_formats_are_sorted_with_translucent_polygons
test_wireframe_translucent_texture_renders_after_opaque_polygons
test_wireframe_decal_translucent_texture_stays_in_opaque_pass
```

Verification:

```sh
cargo test -p nds-core wireframe_translucent_texture --release
cargo test -p nds-core gpu3d::raster --release
cargo test -p nds-core --release
```

Result:

- Wireframe translucent texture targeted release tests: `2 passed; 0 failed`.
- Raster module release tests: `85 passed; 0 failed`.
- `nds-core` full release suite: `498 passed; 0 failed`.

## 2026-06-02 rejected hypothesis: `POLYGON_ATTR` pre-list writes were delayed

Status: **Rejected and reverted**

Direct reference-emulator implementation use for this check: **0**. This came
from re-checking the documented `POLYGON_ATTR` / `BEGIN_VTXS` sequencing,
reading local vertex-state tests, and then checking GBATEK's explicit command
description.

### Symptom

The vertex pipeline treats every `POLYGON_ATTR` write as pending-for-next
`BEGIN_VTXS`, even when no vertex list is active.

That means this valid command order:

```text
POLYGON_ATTR
BEGIN_VTXS
VTX_16 ...
```

works because `BEGIN_VTXS` copies the pending value into the active attribute
slot.

### Root cause

I initially suspected `VertexState::set_polygon_attr()` should apply
immediately when no list was active and defer only during an active list.

That suspicion was wrong. The local test
`test_polygon_attr_snapshot_per_polygon` was encoding the hardware behavior:
`polygon_attr` remains unchanged immediately after a pre-list
`set_polygon_attr()` call, and `BEGIN_VTXS` applies the pending value.

### Spec basis

GBATEK says `POLYGON_ATTR` writes have no effect until the next
`BEGIN_VTXS`, and the `BEGIN_VTXS` section says it additionally applies
changes to `POLYGON_ATTR`.

### Fix

- Reverted the attempted immediate-apply change.
- Kept `VertexState::set_polygon_attr()` writing to `pending_polygon_attr`.
- Kept the snapshot test asserting `polygon_attr == 0` before `BEGIN_VTXS`.
- Updated `docs/concepts/gpu-command-flow.md` to say writes before or during a
  list are staged until the next `BEGIN_VTXS`.

Tests run:

```sh
cargo test -p nds-core gpu3d::vertex --release
cargo test -p nds-core gpu3d --release
cargo test -p nds-core --release
```

Result:

- Vertex pipeline release tests from the temporary patch: `26 passed; 0 failed`.
- GPU3D release tests from the temporary patch: `229 passed; 0 failed`.
- Full-suite verification after reverting: `514 passed; 0 failed`.

## 2026-06-02 fix: texture matrix stack was treated as optional

Status: **Fixed**

Direct reference-emulator implementation use for this fix: **0**. This came
from comparing `MatrixStacks` against ndsdoc's matrix-stack command page and
GBATEK's matrix-stack notes.

### Symptom

The texture matrix stack was modeled as a saved matrix plus a
`texture_saved_valid` flag. `MTX_RESTORE` in texture mode did nothing useful
and set the matrix-stack error flag unless software had previously executed
`MTX_PUSH` or `MTX_STORE`.

That is different from projection mode, where the one-entry stack slot exists
from reset and is initialized to identity.

### Root cause

The code treated the texture saved slot as absent until explicitly initialized.
ndsdoc describes the texture stack as size 1, and for size-1 stacks says
`MTX_STORE` / `MTX_RESTORE` ignore the parameter and use slot 0. GBATEK lists a
hidden texture stack pointer as `0..1`, not a missing stack.

### Fix

- Replaced texture saved-valid semantics with a hidden one-bit
  `texture_sp`.
- Kept `texture_saved` initialized to identity.
- `MTX_RESTORE` in texture mode now always loads the single saved slot and
  leaves the hidden pointer unchanged.
- `MTX_STORE` writes the single saved slot and leaves the hidden pointer
  unchanged.
- `MTX_PUSH` / `MTX_POP` update the hidden one-bit pointer and set overflow on
  the same empty/full-style boundaries as a one-entry stack.
- Clearing the matrix-stack error now resets the hidden texture pointer to 0.

Tests added:

```text
test_texture_restore_uses_single_initialized_stack_slot
test_texture_store_restore_do_not_change_stack_pointer
test_texture_push_pop_uses_hidden_one_bit_stack_pointer
```

Verification:

```sh
cargo test -p nds-core gpu3d::stacks --release
cargo test -p nds-core gpu3d --release
```

Result:

- Stack release tests: `24 passed; 0 failed`.
- GPU3D release tests: `232 passed; 0 failed`.
- `nds-core` full release suite: `517 passed; 0 failed`.

## 2026-06-04 fix: test-busy state did not make GXSTAT geometry-busy

Status: **Fixed**

Direct reference-emulator implementation use for this fix: **0**. This came
from re-reading ndsdoc's readable-matrix note and GBATEK's `GXSTAT` bit layout.

### Symptom class

`GXSTAT` low bit 0 reported Box/POS/VEC test busy, but `GXSTAT` bit 27 did not
include that same busy state. The matrix readback helpers only gate on the
internal geometry-busy predicate, so a future non-instant test-command model
could expose readable matrices while a test command was still executing.

### Spec basis

- ndsdoc says readable matrices require the geometry engine to be disabled via
  `GXSTAT` bit 27.
- GBATEK defines `GXSTAT` bit 0 as Box/POS/VEC test busy and bit 27 as geometry
  engine busy while commands are executing.

Even though the current emulator completes HLE test commands immediately, the
state model should remain internally consistent.

### Fix

- Included `test_busy` in `Engine3d::geometry_busy()`.
- Added `test_test_busy_keeps_geometry_busy_and_blocks_matrix_readback`.

Verification:

```sh
cargo test -p nds-core gpu3d::engine --release
```

Result:

- Engine release tests: `31 passed; 0 failed`.

## 2026-06-04 fix: zero-dot polygons skipped texture/color combine

Status: **Fixed**

Direct reference-emulator implementation use for this fix: **0**. This came
from comparing the local point path against the already-correct filled and
wireframe paths plus GBATEK's rule that alpha testing happens on final polygon
pixels after texture/color blending.

### Symptom class

Degenerate polygons whose vertices collapse to one screen pixel used only the
first vertex color. They ignored:

- `DISP3DCNT` texture enable,
- `TEXIMAGE_PARAM`,
- `PLTT_BASE`,
- texture alpha,
- polygon blend mode,
- toon/highlight combine.

Filled triangles and degenerate line/wireframe paths already used the normal
texture/color combiner, so the point path was inconsistent.

### Why this matters

Small and distant polygons are common during animated 3D scenes. If a collapsed
textured polygon uses raw vertex color instead of the final texture/color
result, it can appear as a wrong-colored single-pixel sparkle or fail alpha
testing differently from hardware.

### Fix

- Changed the zero-dot draw path to receive the full `ScreenPolygon` and VRAM
  context.
- Applied the same texture sampling and polygon-mode combiner used by line and
  filled paths.
- Preserved the existing wireframe alpha rule where `POLYGON_ATTR` alpha 0
  uses opaque wire/point fragments.

Tests added:

```text
test_zero_dot_polygon_samples_texture_when_enabled
test_zero_dot_polygon_uses_vertex_color_when_texture_mapping_disabled
```

Verification:

```sh
cargo test -p nds-core gpu3d::raster::triangle::tests::test_zero_dot --release
```

Result:

- Zero-dot raster release tests: `6 passed; 0 failed`.

## 2026-06-04 tool: deterministic dual-screen PPM capture

Status: **Added**

Direct reference-emulator implementation use for this tool: **0**. This is a
local debugging/conformance aid, not a hardware behavior fix.

### Why this was added

The HeartGold visual state has moved enough that old desktop screenshots are no
longer reliable evidence. Manual screenshots also capture one arbitrary window
frame and include host window scaling. For visual conformance work we need a
repeatable native-size output artifact from the current emulator build.

### Implementation

Added two `nds-frontend` CLI options:

```text
--capture-ppm <PATH>
--capture-frames <N>
```

When `--capture-ppm` is present, the frontend:

- loads the ROM/save/firmware through the normal direct-boot path,
- runs exactly `N` frames (`600` by default),
- writes a binary PPM image at native DS width,
- stacks the top and bottom framebuffers vertically,
- inserts the configured native `--screen-gap`,
- exits without opening SDL audio/video,
- skips `.sav` export on exit so capture runs do not mutate test inputs.

The capture uses the core's current `framebuffer_top` and `framebuffer_bot`
values, converting BGR555 to RGB888.

### Usage

Example:

```sh
./target/release/nds-frontend \
  --rom ~/Documents/Pokemon-HeartGoldVersionUSA.nds \
  --no-audio \
  --capture-frames 900 \
  --capture-ppm /tmp/heartgold-900.ppm
```

This is intended for comparing current output across emulator changes and,
eventually, against reference captures from the same frame window.

### 2026-06-06 extension: PPM image comparator

Added:

```text
tools/compare_ppm.py
tools/compare_ppm_test.py
tools/run_visual_manifest.py
tools/run_visual_manifest_test.py
```

The comparator reads binary `P6` PPM files emitted by `nds-frontend` and
reports:

- image size;
- total pixels;
- changed-pixel count and percentage;
- changed-channel count;
- maximum channel delta;
- RGB RMSE.

It also supports `--write-diff <PATH>`, which writes an amplified PPM diff
image for visual inspection. When both inputs are directories, it compares
matching `frame-000000.ppm` sequence captures and treats `--write-diff` as an
output directory for per-frame diff PPMs. The script has no third-party
dependencies, so it can be used in local debug sessions or CI once reference
captures are checked in or produced by a trusted external runner.

Example exact comparison:

```sh
python3 tools/compare_ppm.py \
  /tmp/current/frame-004320.ppm \
  /tmp/reference/frame-004320.ppm
```

Example sequence comparison:

```sh
python3 tools/compare_ppm.py \
  /tmp/current-heartgold-seq \
  /tmp/reference-heartgold-seq \
  --write-diff /tmp/heartgold-seq-diff
```

Example with a tolerance and diff artifact:

```sh
python3 tools/compare_ppm.py \
  /tmp/current/frame-004320.ppm \
  /tmp/reference/frame-004320.ppm \
  --pixel-threshold 2 \
  --max-changed-pixels 100 \
  --max-channel-delta 8 \
  --write-diff /tmp/frame-004320-diff.ppm
```

This closes the tooling gap between deterministic captures and actual
hardware/reference image comparison. It does not provide the reference images
by itself.

### 2026-06-04 extension: frame sequence capture

Added:

```text
--capture-dir <DIR>
--capture-interval <N>
```

When `--capture-dir` is present, the frontend writes numbered files such as
`frame-000600.ppm` every `N` frames while running up to `--capture-frames`.
This is specifically for transient 3D problems like one-frame polygon flashes:
we can now sample a known frame window from the same ROM/save state after each
GPU3D change without depending on manual desktop screenshots.

Smoke check:

```sh
./target/release/nds-frontend \
  --rom ~/Documents/Pokemon-HeartGoldVersionUSA.nds \
  --no-audio \
  --capture-frames 180 \
  --capture-interval 60 \
  --capture-dir /tmp/heartgold-seq-smoke
```

Result:

- `frame-000060.ppm`
- `frame-000120.ppm`
- `frame-000180.ppm`

All three files were valid 256x392 native dual-screen PPM images.

### 2026-06-06 extension: capture metadata sidecars

Added dependency-free JSON sidecars for frontend capture output:

- single `--capture-ppm /path/frame.ppm` also writes `/path/frame.json`;
- sequence `--capture-dir /path/seq` also writes
  `/path/seq/capture-metadata.json`.

The sidecar format is currently `nds-frontend-capture-v1` and records:

- capture kind (`single` or `sequence`);
- ROM path;
- ROM size;
- ROM title;
- gamecode;
- header CRC status;
- requested frame count;
- capture interval;
- native screen gap;
- source screen dimensions;
- output PPM dimensions;
- sequence frame filenames.

This makes reference-image comparisons less ambiguous: current and reference
captures can be checked for the same frame window and compact/tall layout
before pixel deltas are interpreted.

Smoke check:

```sh
cargo run --release -p nds-frontend -- --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 120 --capture-interval 60 --capture-dir /private/tmp/heartgold-20260606-metadata-smoke-new --capture-ppm /private/tmp/heartgold-20260606-metadata-smoke-new.ppm
```

Result:

- single capture `/private/tmp/heartgold-20260606-metadata-smoke-new.ppm` is a
  valid `256 x 384` PPM;
- single sidecar `/private/tmp/heartgold-20260606-metadata-smoke-new.json`
  reports `kind: single`, `capture_frames: 120`, `capture_interval: 60`,
  `screen_gap: 0`, and `output_height: 384`;
- sequence directory contains `frame-000060.ppm`, `frame-000120.ppm`, and
  `capture-metadata.json`;
- sequence sidecar reports `kind: sequence` and lists both frame files.

### Tests added

```text
test_bgr555_to_rgb888_expands_channels
test_capture_ppm_layout_size
test_capture_args_accept_sequence_options
test_capture_sequence_frames_respects_interval_floor
test_capture_metadata_lists_sequence_frames_and_layout
```

## 2026-06-04 fix: edge marking ignored rear-plane polygon ID

Status: **Fixed**

Direct reference-emulator implementation use for this fix: **0**. This came
from re-reading GBATEK's edge-marking description and comparing it with the
local post-effect implementation.

### Symptom class

The rasterizer initializes `id_buffer` and `depth_buffer` from the rear-plane
state during clear, including the `CLEAR_COLOR` polygon ID. However, the edge
mark post-pass treated any transparent/unwritten neighboring pixel as a generic
different background. That ignored the initialized rear-plane polygon ID.

### Spec basis

GBATEK describes edge marking as comparing the new polygon ID against the old
attribute-buffer ID, and notes that screen-border edges seem to respect the
rear-plane polygon ID from `CLEAR_COLOR`.

### Why this matters

Games can choose rear-plane polygon IDs deliberately. If the emulator always
outlines opaque pixels against transparent rear-plane pixels, it can create
extra outlines around objects whose polygon ID intentionally matches the rear
plane.

### Fix

- Edge marking now compares against `id_buffer` for transparent/unwritten
  in-screen neighbors instead of forcing them to be "different."
- The neighbor depth comes from the initialized depth buffer, so transparent
  rear-plane depth is still honored.
- Off-screen neighbors compare against `CLEAR_COLOR`'s polygon ID, matching
  GBATEK's screen-border note.
- Existing tests that intentionally expect an outline now set a different
  rear-plane polygon ID explicitly instead of relying on the old default-ID
  behavior.

Tests added:

```text
test_edge_marking_respects_transparent_rear_plane_polygon_id
test_edge_marking_outlines_against_different_rear_plane_polygon_id
```

Verification:

```sh
cargo test -p nds-core gpu3d::raster::postfx --release
cargo test -p nds-core gpu3d --release
cargo test --workspace --release
```

Result:

- Post-effect release tests: `19 passed; 0 failed`.
- GPU3D release tests: `237 passed; 0 failed`.
- Workspace release tests: `nds-core 522 passed; nds-frontend 3 passed`.

## 2026-06-04 fix: anti-aliasing also ignored rear-plane polygon ID

Status: **Fixed**

Direct reference-emulator implementation use for this fix: **0**. This was a
local consistency bug found after the edge-marking fix above.

### Symptom class

The anti-aliasing post-pass still treated any transparent/unwritten neighbor as
exposed background. That was inconsistent with the attribute-buffer model used
by the rest of the rasterizer: `clear()` initializes `id_buffer` and
`depth_buffer` from the rear-plane state, including `CLEAR_COLOR`'s polygon ID.

### Why this matters

When a game gives the rear plane the same polygon ID as an object, edge marking
now correctly suppresses that boundary. Anti-aliasing still softened the same
boundary, which could leave unwanted blended silhouettes even after edge
marking was corrected.

### Fix

- Anti-aliasing now checks neighboring `id_buffer` and `depth_buffer` entries
  instead of treating transparent in-screen pixels as generic background.
- Off-screen neighbors compare against `CLEAR_COLOR`'s polygon ID, matching
  the same model used by the edge-marking post-pass.
- The old silhouette test now explicitly sets a different rear-plane polygon
  ID, so it still verifies the intended exposed-edge case.

Tests added:

```text
test_antialias_respects_transparent_rear_plane_polygon_id
test_antialias_softens_against_different_rear_plane_polygon_id
```

Verification:

```sh
cargo test -p nds-core gpu3d::raster::postfx --release
cargo test -p nds-core gpu3d --release
cargo test --workspace --release
```

Result:

- Post-effect release tests: `21 passed; 0 failed`.
- GPU3D release tests: `239 passed; 0 failed`.
- Workspace release tests: `nds-core 524 passed; nds-frontend 3 passed`.

## 2026-06-04 fix: same-ID visible shadows consumed the shadow mask

Status: **Fixed**

Direct reference-emulator implementation use for this fix: **0**. This came
from comparing the local shadow helper against GBATEK's shadow polygon rules.

### Symptom class

Visible shadow polygons (`POLYGON_ATTR.mode = 3`, polygon ID `1..3Fh`) must
draw only when the shadow stencil bit is clear and the incoming shadow polygon
ID differs from the existing attribute-buffer polygon ID. The local helper did
perform the same-ID rejection, but only after checking and clearing the shadow
stencil bit.

That meant a same-ID visible shadow could consume the mask even though it was
not allowed to shade that target pixel.

### Spec basis

GBATEK describes shadow rendering as:

- polygon ID `0` writes the mask and does not draw color;
- polygon ID `1..3Fh` draws normally only when stencil bits are zero;
- step 2 resets stencil bits after checking them;
- rendering additionally requires the incoming shadow polygon ID to differ
  from the ID in the attribute buffer.

The same-ID rejection is a render condition, so it must be checked before the
visible-shadow path consumes the mask.

### Why this matters

Games use matching polygon IDs to prevent an object from casting a shadow onto
itself. If the emulator consumes the mask during that rejection, later shadow
volume passes can see the wrong stencil state. This is a subtle layered-scene
bug rather than a boot blocker, but it affects visual conformance.

### Fix

- `shadow_fragment_is_hidden_or_masked()` now rejects same-ID visible shadows
  before testing and clearing `shadow_stencil`.
- Existing mask behavior for polygon ID `0` is unchanged.
- Existing visible-shadow drawing behavior for different IDs is unchanged.

Test added:

```text
test_visible_shadow_same_id_reject_preserves_mask
```

Verification:

```sh
cargo test -p nds-core shadow --release
cargo test -p nds-core gpu3d --release
cargo test --workspace --release
```

Result:

- Shadow-targeted release tests: `8 passed; 0 failed`.
- GPU3D release tests: `240 passed; 0 failed`.
- Workspace release tests: `nds-core 525 passed; nds-frontend 3 passed`.

## 2026-06-04 fix: SDL display path used darker 5-bit color expansion

Status: **Fixed**

Direct reference-emulator implementation use for this fix: **0**. This was
found by comparing the deterministic capture path with the live SDL display
conversion.

### Symptom class

The deterministic PPM capture path expanded BGR555 channels to RGB888 with bit
replication:

```text
rgb8 = (rgb5 << 3) | (rgb5 >> 2)
```

The SDL display path only shifted channels left by three:

```text
rgb8 = rgb5 << 3
```

That maps full-intensity `31` to `248` instead of `255`, and similarly darkens
intermediate colors. The emulator could therefore show live gameplay slightly
darker than its own captures.

### Why this matters

This is not a GPU command bug, but it is a visual conformance bug in the actual
frontend output. HeartGold uses fades, sky gradients, logo art, and title-scene
lighting where small channel-expansion differences are visible.

### Fix

- Added `expand_bgr555_to_rgb888()` in the SDL video path.
- `DualScreen::convert()` now uses bit-replicating 5-bit to 8-bit expansion,
  matching the capture path.

Test added:

```text
video::tests::test_expand_bgr555_to_rgb888_matches_capture_path
```

Verification:

```sh
cargo test -p nds-frontend --release
cargo test --workspace --release
```

Result:

- Frontend release tests: `4 passed; 0 failed`.
- Workspace release tests: `nds-core 525 passed; nds-frontend 4 passed`.

## 2026-06-04 fix: vertex color interpolation used 5-bit precision

Status: **Fixed**

Direct reference-emulator implementation use for this fix: **0**. This came
from re-reading GBATEK's vertex-color and texture-blending notes and comparing
them with the local raster interpolators.

### Symptom class

GBATEK says 5-bit vertex color components are internally expanded to 6-bit:

```text
if X > 0: X = X*2 + 1
```

The rasterizer stored and interpolated vertex color channels as 5-bit values.
That makes some gradients round down by one framebuffer level before texture
combining or final untextured output.

Example:

```text
red 16 -> red 31 at the midpoint
old 5-bit interpolation: 23
6-bit internal interpolation: 24
```

### Why this matters

HeartGold uses subtle gradients and lit/textured 3D elements in title and intro
scenes. One-level channel errors are small, but they accumulate across fades,
toon/highlight, texture modulation, and live/capture comparison.

### Fix

- `Vert::from()` now expands vertex RGB channels to 6-bit before storing them
  in the scanline interpolator.
- Filled polygon scanlines shrink interpolated 6-bit channels back to 5-bit
  when constructing the BGR555 vertex color.
- Degenerate line interpolation now uses the same internal 6-bit expansion.
- Existing framebuffer format remains BGR555.

Tests added:

```text
test_vertex_color_interpolation_uses_internal_six_bit_channels
test_line_color_interpolation_uses_internal_six_bit_channels
```

Verification:

```sh
cargo test -p nds-core gpu3d::raster::triangle --release
cargo test -p nds-core gpu3d --release
cargo test --workspace --release
```

Result:

- Triangle raster release tests: `57 passed; 0 failed`.
- GPU3D release tests: `242 passed; 0 failed`.
- Workspace release tests: `nds-core 527 passed; nds-frontend 4 passed`.

## 2026-06-04 coverage: 3D BG0 ignores vertical scroll

Status: **Covered**

Direct reference-emulator implementation use for this coverage: **0**. This
came from GBATEK's final 3D-to-2D output notes.

### Behavior

When the 3D renderer is mapped to Engine A BG0, BG0 horizontal scroll applies
with a 512-pixel source region, but vertical scrolling does not apply. The 3D
layer always uses the physical scanline currently being composited.

The implementation already behaved this way because the 3D BG0 synthesis path
uses `line` directly and only applies `BG0HOFS`.

### Why this matters

Commercial games freely mix 2D backgrounds, OBJs, and 3D BG0. Accidentally
treating 3D BG0 like a normal vertically scrollable text/bitmap BG would shift
the entire 3D layer relative to 2D overlays.

### Test added

```text
gpu2d::tests::test_3d_bg0_ignores_vertical_scroll
```

Verification:

```sh
cargo test -p nds-core gpu2d --release
cargo test --workspace --release
```

Result:

- GPU2D release tests: `7 passed; 0 failed`.
- Workspace release tests: `nds-core 528 passed; nds-frontend 4 passed`.

## 2026-06-04 fix: Engine A display-capture register and scanline capture subset

Status: **Implemented as prerequisite coverage**

Direct reference-emulator implementation use for this fix: **0**. This came
from GBATEK's `DISPCAPCNT` description and the existing local video pipeline.

### Symptom / suspicion

The old random-polygon title-page failure is no longer reproduced in current
fixed-frame captures, but HeartGold still reaches a title/intro area where one
screen can be black while the other shows the scene. Commercial DS games often
use display capture for title transitions, motion trails, 3D-to-2D feedback, or
VRAM-display effects.

Before this change, `0x04000064..0x04000067` was deliberately excluded from
normal Engine A register decoding but was not handled elsewhere. Writes to
`DISPCAPCNT` therefore fell through to unhandled I/O, and the core had no way
to copy Engine A/3D output into LCDC VRAM for later display.

### Fix

- Added shared-state storage for `DISPCAPCNT`.
- Modeled pending vs active capture state:
  - setting bit 31 arms capture for the next visible line 0;
  - capture runs during visible scanlines;
  - bit 31 clears after line 191.
- Implemented scanline capture into LCDC VRAM for Engine A:
  - source A = Engine A graphics output;
  - source A = raw 3D framebuffer when `DISPCAPCNT[24]` is set;
  - source B = LCDC VRAM readback;
  - source A only, source B only, and source A+B blended modes;
  - capture sizes `128x128`, `256x64`, `256x128`, and `256x192`;
  - VRAM write block and read/write offsets.
- Preserved capture alpha in bit 15 for later capture blending, while display
  mode 2 continues to show only BGR555 color.

At this point, using main-memory display FIFO as `DISPCAPCNT[25]` source B was
still not implemented. That was closed in the follow-up section below.

Tests added:

```text
test_dispcapcnt_readback_and_enable_arms_next_frame_capture
test_display_capture_source_a_writes_engine_a_line_to_lcdc_vram
```

Verification:

```sh
cargo test -p nds-core dispcap --release
cargo test -p nds-core display_capture --release
cargo test --workspace --release
```

Result:

- `dispcap` focused test: `1 passed; 0 failed`.
- `display_capture` focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 530 passed; nds-frontend 4 passed`.

Fresh HeartGold capture after this change:

```sh
./target/release/nds-frontend \
  --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds \
  --no-audio \
  --capture-frames 1800 \
  --capture-dir /tmp/heartgold-dispcap-seq \
  --capture-interval 900
```

Result:

- Frame 1800 still reaches the stable title/intro scene.
- No random polygon flashing was observed in the fixed capture.
- This change did **not** visibly alter the inspected frame 1800 output, so it
  should be considered display-system groundwork rather than proof that the
  remaining HeartGold title-screen mismatch is solved.

## 2026-06-04 fix: Main-memory display FIFO and Engine A display mode 3

Status: **Implemented as display-system prerequisite coverage**

Direct reference-emulator implementation use for this fix: **0**. This came
from GBATEK's `DISP_MMEM_FIFO` notes and the local DMA/display architecture.

### Symptom / suspicion

HeartGold's intro now advances through several title/animation phases without
the old random-polygon flashing, but the remaining visual uncertainty is around
commercial display composition paths rather than raw polygon emission. The DS
has a main-memory display FIFO that can feed Engine A display mode 3 directly,
and that FIFO can also participate in display capture effects.

Before this change:

- `DISPCNT` display mode 3 fell through to normal compositing.
- `DISP_MMEM_FIFO` at `0x04000068` was not stored.
- ARM9 DMA timing mode 4 existed in the DMA decoder, but no frame/display hook
  consumed it for main-memory display.

### Fix

- Added a `DISP_MMEM_FIFO` queue to shared state.
- Added 32-bit writes to `0x04000068`, storing two BGR555 pixels per word.
- Implemented Engine A display mode 3 by consuming 128 FIFO words per scanline
  and writing 256 BGR555 pixels.
- Added a scheduler hook that, before rendering an Engine A main-memory display
  scanline, repeatedly services ARM9 DMA3 timing mode 4 until the FIFO has a
  full scanline or the DMA stops.
- Added a DMA special-case so a DMA destination latched from `0x04000068`
  reaches the FIFO despite the existing ARM9 DMA local-address masking.

The implementation intentionally keeps the first slice narrow: it models the
display path and DMA feed, but does not yet use the FIFO as display-capture
source B.

Tests added:

```text
test_main_memory_display_mode_consumes_fifo_pixels
test_main_memory_display_fifo_dma_feeds_scanline
```

Verification:

```sh
cargo test -p nds-core main_memory_display --release
cargo test --workspace --release
```

Result:

- Main-memory display focused tests: `2 passed; 0 failed`.
- Workspace release tests: `nds-core 532 passed; nds-frontend 4 passed`.

Fresh HeartGold capture after this change:

```sh
./target/release/nds-frontend \
  --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds \
  --no-audio \
  --capture-frames 3600 \
  --capture-dir /tmp/heartgold-mmemfifo-3600 \
  --capture-interval 1200
```

Result:

- The intro still advances to the frame-3600 animation sample.
- No obvious display regression was observed in the inspected capture.
- The frame looked the same as the previous current-build sample, so this is
  best counted as removing a known missing hardware path rather than solving a
  newly observed HeartGold visual difference.

## 2026-06-04 fix: Display capture source B can consume main-memory FIFO

Status: **Implemented as display-capture conformance coverage**

Direct reference-emulator implementation use for this fix: **0**. This came
from GBATEK's `DISPCAPCNT` source-B description and the local FIFO/DMA path.

### Symptom / gap

The previous display-capture subset handled source B when it came from LCDC
VRAM, but `DISPCAPCNT[25]` selects the main-memory display FIFO instead. The
core already had a FIFO queue and DMA3 timing mode 4 after the preceding fix,
but capture source B still returned transparent black.

That left an important commercial display-effect path incomplete:

- Engine A can display FIFO-fed main-memory bitmaps directly.
- Display capture can also use the same FIFO as source B.
- Source A+B capture blending depends on source B having real color/alpha.

### Fix

- Capture source B now consumes `DISP_MMEM_FIFO` words when `DISPCAPCNT[25]`
  is set.
- Each FIFO word supplies two 15-bit BGR pixels.
- FIFO pixels are treated as solid for capture alpha because GBATEK documents
  the FIFO pixel format as 15-bit RGB with bit 15 unused.
- The scheduler now feeds DMA3 main-memory-display FIFO before capture lines
  that select FIFO source B, even when Engine A itself is not in display mode 3.

Tests added:

```text
test_display_capture_source_b_consumes_main_memory_fifo
test_display_capture_source_b_dma_feeds_main_memory_fifo
```

Verification:

```sh
cargo test -p nds-core display_capture --release
cargo test --workspace --release
```

Result:

- Display-capture focused tests: `3 passed; 0 failed`.
- Workspace release tests: `nds-core 534 passed; nds-frontend 4 passed`.

Fresh HeartGold capture after this change:

```sh
cargo build -p nds-frontend --release
./target/release/nds-frontend \
  --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds \
  --no-audio \
  --capture-frames 3600 \
  --capture-dir /tmp/heartgold-capturefifo-3600 \
  --capture-interval 1200
```

Result:

- Frame 3600 still reaches the intro animation sample.
- No new obvious display regression was observed in the inspected capture.

## 2026-06-04 fix: Display capture VRAM read/write offsets wrap within block

Status: **Implemented as display-capture conformance coverage**

Direct reference-emulator implementation use for this fix: **0**. This came
from GBATEK's `DISPCAPCNT` note that VRAM read/write offsets wrap to `0` when
exceeding `0x1FFFF`.

### Symptom / gap

Display capture can start reading or writing at offsets `0x00000`, `0x08000`,
`0x10000`, or `0x18000` within a selected 128 KB VRAM block. A full
`256x192` capture is `0x18000` bytes, so starting at offset `0x18000` crosses
the end of the block halfway through the capture.

Before this change, the emulator added block + offset + pixel position directly.
When that exceeded the selected bank's 128 KB span, LCDC VRAM routing ignored
the write/read instead of wrapping within the block.

### Fix

- Added a capture VRAM address helper:

```text
block * 0x20000 + ((offset + byte_pos) & 0x1FFFF)
```

- Applied it to display-capture destination writes.
- Applied it to source-B VRAM reads when `DISPCAPCNT[25] = 0`.

Tests added:

```text
test_display_capture_write_offset_wraps_within_vram_block
test_display_capture_source_b_vram_read_offset_wraps_within_block
```

Verification:

```sh
cargo test -p nds-core display_capture --release
cargo test --workspace --release
```

Result:

- Display-capture focused tests: `5 passed; 0 failed`.
- Workspace release tests: `nds-core 536 passed; nds-frontend 4 passed`.

## 2026-06-04 coverage: Display capture source A+B blend formula

Status: **Covered**

Direct reference-emulator implementation use for this coverage: **0**. This
came from GBATEK's `DISPCAPCNT` capture A+B formula.

### Behavior

When `DISPCAPCNT[29:30]` selects source A+B blending, the capture unit blends
each 5-bit color channel using:

```text
Dest = (SrcA * SrcAAlpha * EVA + SrcB * SrcBAlpha * EVB) / 16
```

The destination alpha bit is set only when a contributing source is present and
its corresponding factor is nonzero:

```text
DestAlpha = (SrcAAlpha AND EVA > 0) OR (SrcBAlpha AND EVB > 0)
```

### Why this matters

Commercial games can use capture blending for fades, motion trails, feedback
effects, and mixed 3D/2D intro transitions. A wrong alpha gate can leave stale
captured pixels visible or make a capture disappear when a source is
transparent.

The existing implementation already matched this formula, but it was not
directly covered by tests.

Tests added:

```text
test_display_capture_blends_source_a_and_fifo_source_b
test_capture_blend_ignores_transparent_sources_and_gates_alpha
```

Verification:

```sh
cargo test -p nds-core display_capture --release
cargo test -p nds-core capture_blend --release
cargo test --workspace --release
```

Result:

- Display-capture focused tests: `6 passed; 0 failed`.
- Capture-blend focused tests: `2 passed; 0 failed`.
- Workspace release tests: `nds-core 538 passed; nds-frontend 4 passed`.

## 2026-06-05 fix: VRAM display mode ignores capture source-B read offset

Status: **Implemented as display-capture conformance coverage**

Direct reference-emulator implementation use for this fix: **0**. This came
from GBATEK's `DISPCAPCNT` note that the VRAM read offset is ignored when the
display mode is VRAM display mode.

### Symptom / gap

The display-capture source-B path uses `DISPCAPCNT[26:27]` as a VRAM read
offset when source B is VRAM. GBATEK specifies that this read offset is ignored
when `DISPCNT[16:17]` selects VRAM display mode.

Before this change, capture always applied the source-B read offset. That means
a capture configured while Engine A was in VRAM display mode could read from
`0x08000`, `0x10000`, or `0x18000` instead of the selected block's start.

### Fix

- Source-B VRAM capture now forces read offset to `0` when Engine A display
  mode is VRAM display mode.
- Non-VRAM-display capture still uses the programmed read offset, with the
  existing within-block wrapping.

Test added:

```text
test_display_capture_vram_display_mode_ignores_source_b_read_offset
```

Verification:

```sh
cargo test -p nds-core display_capture --release
cargo test --workspace --release
```

Result:

- Display-capture focused tests: `7 passed; 0 failed`.
- Workspace release tests: `nds-core 539 passed; nds-frontend 4 passed`.

## 2026-06-05 fix: ARM9 byte writes to 3D render registers preserve adjacent bytes

Status: **Implemented as 3D render-register conformance fix**

Direct reference-emulator implementation use for this fix: **0**. This came
from inspecting the local ARM9 IO dispatcher against GBATEK's 3D rendering
register map. The relevant registers are write-only or table-like, so byte
writes must update the emulator's stored value directly instead of using a
read-modify-write path that reads back zero.

### Symptom / gap

`write_io8` handled unrecognized registers by reading the containing halfword,
replacing one byte, and writing the halfword back. That works for readable IO
registers, but the 3D rendering tables/registers at `0x04000330..0x040003BF`
are mostly write-only in the current read path.

Before this change, a byte write to the high byte of one of these registers
could erase the low byte:

```text
write16 FOG_TABLE[0..1] = AABB
write8  FOG_TABLE[1]    = CC
old result: 00CC
new result: BBCC
```

The same class of bug applied to `EDGE_COLOR`, `CLEAR_COLOR`, `CLEAR_DEPTH`,
`CLRIMAGE_OFFSET`, `FOG_COLOR`, `FOG_OFFSET`, and `TOON_TABLE`.

### Fix

- Added direct `write_io8` handling for the ARM9 3D rendering register block:
  - `EDGE_COLOR`
  - `ALPHA_TEST_REF`
  - `CLEAR_COLOR`
  - `CLEAR_DEPTH`
  - `CLRIMAGE_OFFSET`
  - `FOG_COLOR`
  - `FOG_OFFSET`
  - `FOG_TABLE`
  - `TOON_TABLE`
- Kept 15-bit color masking for `EDGE_COLOR` and `TOON_TABLE` high-byte writes.
- Left the generic read-modify-write fallback for ordinary readable registers.

### Why this matters

Commercial games can update fog/toon/edge/clear data through byte stores. If
the paired byte is silently cleared, post-effects and toon/edge colors can
change even though the game only intended to touch one entry byte.

Tests added:

```text
test_arm9_3d_render_register_byte_writes_preserve_neighbor_bytes
test_3d_bg0_ignores_bgcnt_non_priority_bits
test_3d_bg0_second_target_uses_bldalpha_not_3d_alpha
test_3d_bg0_first_target_supports_brightness_effects
test_disp_1dot_depth_uses_any_vertex_w_to_keep_zero_dot_polygon
test_depth_equal_uses_hardware_tolerance
test_a3i5_alpha_expands_to_five_bits
test_4color_color0_transparent
test_16color_color0_transparent
test_4x4_compressed_slot2_uses_upper_slot1_params
test_rear_bitmap_clear_uses_texture_slots_and_scroll
test_toon_highlight_rgb_uses_hardware_formula
test_decal_mid_alpha_uses_six_bit_ratio_formula
test_edge_marking_uses_polygon_id_color_group_and_masks_bit15
test_translucent_polygon_overwrites_transparent_framebuffer
test_translucent_blend_updates_alpha_buffer_to_max
test_opaque_polygon_overwrites_when_alpha_blend_enabled
```

Verification:

```sh
cargo test -p nds-core test_arm9_3d_render_register_byte_writes_preserve_neighbor_bytes --release
cargo test -p nds-core test_3d_bg0_ignores_bgcnt_non_priority_bits --release
cargo test -p nds-core test_3d_bg0_second_target_uses_bldalpha_not_3d_alpha --release
cargo test -p nds-core test_3d_bg0_first_target_supports_brightness_effects --release
cargo test -p nds-core test_disp_1dot_depth_uses_any_vertex_w_to_keep_zero_dot_polygon --release
cargo test -p nds-core test_depth_equal_uses_hardware_tolerance --release
cargo test -p nds-core test_a3i5_alpha_expands_to_five_bits --release
cargo test -p nds-core color0_transparent --release
cargo test -p nds-core compressed --release
cargo test -p nds-core test_rear_bitmap_clear_uses_texture_slots_and_scroll --release
cargo test -p nds-core test_toon_highlight_rgb_uses_hardware_formula --release
cargo test -p nds-core test_decal_mid_alpha_uses_six_bit_ratio_formula --release
cargo test -p nds-core test_edge_marking_uses_polygon_id_color_group_and_masks_bit15 --release
cargo test -p nds-core test_translucent_polygon_overwrites_transparent_framebuffer --release
cargo test -p nds-core test_translucent_blend_updates_alpha_buffer_to_max --release
cargo test -p nds-core test_opaque_polygon_overwrites_when_alpha_blend_enabled --release
cargo test --workspace --release
```

Result:

- 3D render-register byte-write focused test: `1 passed; 0 failed`.
- 3D BG0 BG0CNT focused test: `1 passed; 0 failed`.
- 3D BG0 second-target blend focused test: `1 passed; 0 failed`.
- 3D BG0 brightness focused test: `1 passed; 0 failed`.
- 0-dot W-boundary focused test: `1 passed; 0 failed`.
- Depth-equal tolerance focused test: `1 passed; 0 failed`.
- A3I5 alpha expansion focused test: `1 passed; 0 failed`.
- 4/16/256-color color-0 transparency focused tests: `3 passed; 0 failed`.
- 4x4 compressed texture focused tests: `3 passed; 0 failed`.
- Rear-plane bitmap focused test: `1 passed; 0 failed`.
- Toon/highlight RGB formula focused test: `1 passed; 0 failed`.
- Decal mid-alpha formula focused test: `1 passed; 0 failed`.
- Edge-color group focused test: `1 passed; 0 failed`.
- Transparent-framebuffer alpha-blend bypass focused test: `1 passed; 0 failed`.
- Alpha-buffer max focused test: `1 passed; 0 failed`.
- Opaque-polygon alpha-blend bypass focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 555 passed; nds-frontend 4 passed`.

### Additional coverage: 0-dot polygon W comparison

GBATEK notes that `DISP_1DOT_DEPTH` checks the W coordinates of all vertices,
but the 0-dot polygon is still rendered using the first vertex's
color/depth/texture data. Added coverage for a polygon where the first vertex
is behind the boundary, a later vertex is within the boundary, and all vertices
land on the same screen pixel. This locks in the existing all-vertices W check
and protects against regressing to first-vertex-only culling.

### Additional coverage: depth-equal tolerance

GBATEK documents `POLYGON_ATTR.Bit14` depth-equal mode as allowing matches
within `+/-0x200` in the 24-bit depth range, not only exact equality. Added a
boundary test for exact equality, both inclusive `0x200` edges, and both
exclusive `0x201` edges. The same test also confirms normal depth-less mode
does not accept equal depth.

### Additional coverage: 3D BG0 as blend second target

GBATEK describes special-effects behavior for the final 3D output: when BG0/3D
is the first blend target, per-pixel 3D alpha is used; when BG0/3D is the
second target, normal `BLDALPHA` EVA/EVB blending is used like other 2D
layers. Added coverage where BG1 is first target, BG0/3D is second target, and
the 3D pixel's alpha is intentionally set to a value that would produce a
different result if it were incorrectly used.

### Additional coverage: 3D BG0 brightness effects

GBATEK also lists brightness increase/decrease with BG0 as first target as
normal 2D special effects for the final 3D output. Added coverage that routes
BG0/3D through `BLDY` brightness-up and brightness-down modes, confirming that
3D BG0 participates in those first-target effects just like a normal BG layer.

### Additional coverage: A3I5 alpha expansion

GBATEK specifies that A3I5 texture alpha expands from 3-bit to 5-bit with
`Alpha=(Alpha*4)+(Alpha/2)`. Added sampler coverage for representative values
`0`, `1`, `4`, and `7`, proving they produce 5-bit alpha values `0`, `4`,
`18`, and `31`. This protects translucent textured polygons from regressions
in the texture-fetch path.

### Additional coverage: indexed texture color-0 transparency

GBATEK applies the `TEXIMAGE_PARAM.Bit29` color-0 transparency flag to the
4-color, 16-color, and 256-color indexed texture formats. Existing coverage
already checked the 256-color case; added 4-color and 16-color fixtures that
sample palette index `0` as transparent and palette index `1` as opaque. The
production sampler already used the shared indexed-texture path correctly.

### Additional coverage: 4x4 compressed texture Slot 2 parameters

GBATEK maps 4x4 compressed texture blocks in Slot 2 to compressed parameter
entries in the upper 64 KiB of texture-image Slot 1:
`slot1_addr = slot2_addr / 2 + 10000h`. Added a Slot 2 fixture whose texel
index `3` is opaque only when the sampler reads mode `2` from that upper-half
parameter entry. Reading the lower-half Slot 0 parameter area would incorrectly
leave the texel transparent. The production sampler already matched this
addressing rule.

### Extended coverage: rear-plane bitmap alpha and fog/depth separation

GBATEK's rear-plane bitmap mode uses texture Slot 2 for color and texture Slot
3 for depth. The color bitmap has only a 1-bit alpha flag, while depth bitmap
bit 15 is the initial fog flag and must not contribute to the 15-bit clear
depth value. Extended the existing rear-plane bitmap test to assert both alpha
states and to prove a `0xFFFF` depth bitmap entry expands as clear depth
`0x7FFF` with fog enabled, not as a larger depth value.

### Additional coverage: toon/highlight RGB blend formula

GBATEK defines toon and highlight shading in 6-bit expanded channel space:
toon mode multiplies texture and toon-table channels, while highlight mode
adds the toon-table shade after that multiply and clamps before shrinking back
to 5-bit color. Added a direct formula test with mid-intensity texture and
shade values. The same input now proves toon red output `9` and highlight red
output `25`, protecting textured title graphics from subtle color/intensity
regressions.

### Additional coverage: decal mid-alpha ratio formula

GBATEK specifies decal mode as using texture alpha only as the RGB mix ratio;
the final output alpha still comes from `POLYGON_ATTR`. Existing tests covered
only the `At=0` and `At=31` shortcuts. Added a mid-alpha texture sample that
proves `At=16` is expanded to the 6-bit ratio before mixing, producing red
channel `16` over a black vertex color while leaving the helper's fragment
alpha at the opaque placeholder used before `final_alpha` applies polygon
alpha.

### Additional coverage: edge-color group selection

GBATEK maps edge colors by polygon ID group: IDs `00h..07h` use
`EDGE_COLOR[0]`, IDs `08h..0Fh` use `EDGE_COLOR[1]`, and so on. Added a
post-effect fixture for polygon ID `8` that must select `EDGE_COLOR[1]` and
mask off the table entry's unused bit 15. This protects outlines on objects
using nonzero polygon-ID groups.

### Additional coverage: alpha-blend bypass on transparent framebuffer

GBATEK says translucent polygon blending is bypassed when the old framebuffer
pixel has alpha `0`; the new polygon color/alpha is written directly instead
of blending against the transparent rear plane. Added a rendered triangle test
with alpha blending enabled and the default transparent rear plane. A red
alpha-16 polygon must write full red color with alpha buffer `16`, rather than
half-red from blending against black.

### Additional coverage: framebuffer alpha max on translucent blending

GBATEK specifies blended framebuffer alpha as `max(Poly[A], FrameBuf[A])`.
Added a two-layer translucent overlap test with manual order: the first
fragment writes alpha `8` over a transparent rear plane, and the second
different-ID fragment blends over it with alpha `16`. The resulting alpha
buffer must become `16`, proving the emulator preserves the max-alpha rule for
translucent-over-translucent pixels.

### Additional coverage: alpha-blend bypass for opaque polygon pixels

GBATEK says alpha blending is bypassed when `Poly[A]=31`, even if
`DISP3DCNT.Bit3` enables alpha blending. Added an overlap test with a far blue
opaque polygon and a nearer red opaque polygon while alpha blending is enabled.
The output must be full red with alpha buffer `31`, not a blended color.

## 2026-06-06 status: current HeartGold capture reaches title screen

Status: **Boot path past title-screen blocker; remaining work is visual conformance**

Current verification command used the already-built release binary, avoiding a
recompile on every manual run:

```sh
target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-dir /private/tmp/heartgold-captures-long --capture-frames 4800 --capture-interval 600
```

Result:

- Direct boot loaded `POKEMON HG` / `IPKE`.
- Existing save loaded from `/Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.sav`.
- Sparse captures through frame `4800` reached the interactive title screen.
- Frame `4200` shows the top-screen Ho-Oh scene with `TOUCH TO START` and the
  bottom-screen HeartGold logo.
- Frame `4800` shows the same title scene without the prompt phase.

The previous Desktop screenshot `Screenshot 2026-06-01 at 11.57.19 PM.png`
should now be treated as a stale baseline for the boot/flicker blocker. It
matches the broad title-screen state rather than showing the earlier black
screen or random-polygon failure. The next useful debugging target is not card
I/O or NitroFS; it is remaining 3D visual conformance, especially polygon
edge/fill behavior, interpolation, depth ordering, and post-effects on the
top-screen title model.

## 2026-06-06 coverage: SWAP_BUFFERS manual translucent sort affects rendered output

Status: **Added focused 3D order-conformance coverage**

Direct reference-emulator implementation use for this test: **0**. This came
from GBATEK's `SWAP_BUFFERS` bit definition:

- Bit 0 clear: translucent polygon Y-sorting is automatic.
- Bit 0 set: translucent polygon sorting is manual, preserving software order.

The rasterizer already stored this bit as `manual_translucent_sort`, but the
existing tests did not prove that it changes final pixels. Added a focused
overlap test with two alpha-16 translucent triangles whose software order
conflicts with the automatic Y-sort key. The automatic and manual paths must
produce different blended colors, and the manual path must leave the later
software polygon as the stronger color contribution.

Test added:

```text
test_swap_buffers_manual_sort_preserves_translucent_software_order
```

Verification:

```sh
cargo test -p nds-core test_swap_buffers_manual_sort_preserves_translucent_software_order --release
cargo test --workspace --release
```

Result:

- Manual translucent sort focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 556 passed; nds-frontend 4 passed`.

## 2026-06-06 fix: texture-alpha formats are opaque when texture mapping is disabled

Status: **Implemented as 3D render-order conformance fix**

Direct reference-emulator implementation use for this fix: **0**. This came
from comparing the rasterizer's translucent-pass classification with
`DISP3DCNT.Bit0` and GBATEK's texture-blending rules. A3I5 and A5I3 texture
formats carry texel alpha only when texture mapping is enabled. If texture
mapping is disabled, an alpha-31 polygon using one of those texture formats
renders from vertex color with opaque alpha and must stay in the opaque pass.

### Symptom / gap

`is_translucent` classified modulation/toon polygons with A3I5/A5I3 texture
formats as translucent without checking whether texture mapping was enabled.
That could delay an otherwise opaque polygon into the translucent pass, changing
render order and depth behavior when a game temporarily disabled texture
mapping through `DISP3DCNT.Bit0`.

### Fix

- Threaded the rasterizer's texture-mapping enable bit into the translucent
  classification used by `render_frame`.
- Kept polygon alpha `1..30` classified as translucent regardless of texture
  state.
- Classified A3I5/A5I3 alpha-31 modulation/toon polygons as texture-alpha
  translucent only when texture mapping is enabled.

Test added:

```text
test_translucent_texture_format_is_opaque_when_texture_mapping_disabled
```

Verification:

```sh
cargo test -p nds-core translucent_texture --release
cargo test --workspace --release
```

Result:

- Texture/translucency focused tests: `7 passed; 0 failed`.
- Workspace release tests: `nds-core 557 passed; nds-frontend 4 passed`.

## 2026-06-06 fix: texture-disabled alpha formats do not force translucent fill rule

Status: **Implemented as 3D raster fill-rule conformance fix**

Direct reference-emulator implementation use for this fix: **0**. This is the
same texture-enable distinction as the render-order fix above, applied to the
scan converter's small-polygon edge rule.

### Symptom / gap

The rasterizer uses GBATEK's lower/right edge exclusion rule for small polygons:
opaque polygons without edge-marking or anti-aliasing are shrunken, translucent
polygons are shrunken when alpha blending is disabled, and vertical right edges
are still excluded. The local implementation treated A3I5/A5I3 texture formats
as translucent for that rule even when `DISP3DCNT.Bit0` disabled texture
mapping.

That was wrong when anti-aliasing was enabled and texture mapping was disabled:
an alpha-31 A5I3/A3I5 polygon should behave as opaque vertex-color geometry,
so a non-vertical right edge should remain included.

### Fix

- Added the texture-mapping enable gate to `uses_small_polygon_fill_rule`.
- A3I5/A5I3 texture formats only contribute "translucent texture" behavior to
  edge exclusion when texture sampling is actually enabled.

Test added:

```text
test_disabled_texture_alpha_format_does_not_force_translucent_fill_rule
```

Verification:

```sh
cargo test -p nds-core test_disabled_texture_alpha_format_does_not_force_translucent_fill_rule --release
cargo test --workspace --release
```

Result:

- Disabled-texture fill-rule focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 558 passed; nds-frontend 4 passed`.

## 2026-06-06 fix: canonical masks for write-only 3D render register state

Status: **Implemented as 3D render-register conformance fix**

Direct reference-emulator implementation use for this fix: **0**. This came
from GBATEK's bit layouts for the rendering-engine registers and the local
write paths added earlier for byte writes.

### Symptom / gap

The emulator already masked unused bits at many use sites, but the stored
write-only register state could still retain unused bits:

- `CLEAR_COLOR` bits 21..23 and 30..31
- `CLEAR_DEPTH` bit 15
- `FOG_COLOR` bit 15 and bits 21..31
- `FOG_OFFSET` bit 15
- `FOG_TABLE` bit 7 for each density entry

That did not usually affect current rendering because later code masked the
fields again, but it left non-hardware state in save states and made byte-write
preservation tests less precise than the register definitions.

### Fix

- Mask `CLEAR_COLOR` to the hardware-defined color/fog/alpha/polygon-ID bits
  after byte and halfword writes.
- Mask `CLEAR_DEPTH` and `FOG_OFFSET` to 15 bits after byte and halfword writes.
- Mask `FOG_COLOR` to color plus fog-alpha bits after byte and halfword writes.
- Mask each `FOG_TABLE` byte to its 7-bit density field for byte and halfword
  writes.
- Extended the existing ARM9 3D render-register byte-write test so it now
  verifies both neighbor-byte preservation and unused-bit masking.

Verification:

```sh
cargo test -p nds-core test_arm9_3d_render_register_byte_writes_preserve_neighbor_bytes --release
cargo test --workspace --release
```

Result:

- ARM9 render-register byte-write/mask focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 558 passed; nds-frontend 4 passed`.

## 2026-06-06 check: quad fan diagonal coverage

Status: **Ruled out as the current HeartGold title artifact**

Direct reference-emulator implementation use for this check: **0**. This was a
local hypothesis from the rasterizer architecture plus GBATEK's note that the
DS supports native triangles and quadliterals.

### Hypothesis

The NDS rasterizer accepts native 4-vertex quads, while the current software
rasterizer triangulates every polygon as a fan around vertex 0. If the lower /
right edge exclusion rule were applied to the artificial diagonal between the
two fan triangles, a quad could show a missing-pixel seam. That kind of seam
would be visible in title-screen polygon art and could look like flashing
polygon corruption when animated.

### Result

Added a focused regression test that renders an opaque axis-aligned quad
through the current fan path and samples pixels on the fan diagonal:

```text
test_quad_fan_does_not_leave_internal_diagonal_gap
```

The test passes. That means the current fan triangulation does not leave a
simple internal diagonal coverage hole for this case. It does not prove native
quad interpolation is fully hardware-accurate, but it rules out the simplest
coverage-seam explanation for the remaining HeartGold title-screen artifacts.

Verification:

```sh
cargo test -p nds-core test_quad_fan_does_not_leave_internal_diagonal_gap --release
cargo test --workspace --release
```

Result:

- Quad fan diagonal focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 559 passed; nds-frontend 4 passed`.

## 2026-06-06 fix: default dual-screen gap removed

Status: **Implemented as frontend layout fix**

Direct reference-emulator implementation use for this fix: **0**. This came
from inspecting the local frontend capture/window layout after the current
HeartGold frame was coherent but still showed a large separator between
screens.

### Symptom / gap

The command:

```sh
target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 4200 --capture-ppm /private/tmp/heartgold-compact-gap.ppm
```

still produced a 256x392 capture before rebuilding the release binary. That
height is `192 + 8 + 192`, proving the visible gap was not a rendering bug in
the core; it was the frontend's default `DEFAULT_SCREEN_GAP = 8` native pixels.
At `--scale 2`, that default becomes a 16-pixel window separator.

### Fix

- Changed the frontend default screen gap from 8 native pixels to 0.
- Kept `--screen-gap` available for explicit DS-style separation.
- Added `test_screen_gap_defaults_to_compact_layout` so the default remains
  compact.

Verification:

```sh
cargo test -p nds-frontend --release
cargo build --release -p nds-frontend
target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 4200 --capture-ppm /private/tmp/heartgold-compact-gap.ppm
cargo test --workspace --release
```

Result:

- Frontend release tests: `5 passed; 0 failed`.
- Rebuilt `target/release/nds-frontend`.
- New HeartGold frame-4200 capture is `256 x 384`, which is exactly two
  256x192 screens with no separator.
- Workspace release tests: `nds-core 559 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: texture-coordinate transform mode formulas

Status: **Added focused 3D texture-transform conformance coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from GBATEK's texture-coordinate transformation formulas.

### Why this matters

Commercial model/title-screen assets often rely on texture matrices for
scrolling, projection-like effects, and reflection mapping. A row/column or
fixed-point mistake in these formulas can make otherwise correct polygons show
wrong or unstable texture placement.

### Coverage added

- `test_texcoord_transform_mode_0_ignores_texture_matrix` proves mode 0 keeps
  raw `TEXCOORD` values even when the texture matrix contains translation.
- `test_texcoord_transform_mode_1_uses_one_sixteenth_matrix_terms` proves mode
  1 uses GBATEK's `(S, T, 1/16, 1/16)` input vector, including the documented
  contribution from matrix row `m[8]/m[9]`.

The existing implementation already matched this behavior; this change locks
it down while continuing the 3D visual-conformance audit.

Verification:

```sh
cargo test -p nds-core texcoord_transform_mode_ --release
cargo test --workspace --release
```

Result:

- Texture-coordinate transform focused tests: `7 passed; 0 failed`.
- Workspace release tests: `nds-core 561 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: visible shadow alpha controls intensity

Status: **Added focused 3D shadow conformance coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from GBATEK's shadow-polygon notes: visible shadow polygons use
`POLYGON_ATTR` alpha as the shadow intensity, while polygon mode 3 and nonzero
polygon ID control the shadow rendering pass.

### Why this matters

If visible shadow polygons ignored polygon alpha, commercial scenes would show
hard black/colored overlays instead of translucent shadows. That would be a
large visual mismatch even when geometry, depth, and texture sampling are
otherwise correct.

### Coverage added

`test_visible_shadow_uses_polygon_alpha_as_intensity` renders an opaque blue
base polygon, then a nearer polygon-mode-3 visible shadow with polygon ID 2 and
alpha 16. With alpha blending enabled, the result must be the normal
alpha-blend of black shadow color over the blue surface, while the framebuffer
alpha remains the max of the two fragments.

The existing implementation already matched this behavior; this change locks
it down as part of the visual-conformance audit.

Verification:

```sh
cargo test -p nds-core test_visible_shadow_uses_polygon_alpha_as_intensity --release
cargo test --workspace --release
```

Result:

- Visible-shadow alpha focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 562 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: W-buffer mode orders pixels by W

Status: **Added focused 3D depth-order conformance coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from GBATEK's `SWAP_BUFFERS.Bit1` definition: depth buffering can use either
Z values or W values, and fog depth follows the active depth-buffer mode.

### Why this matters

If W-buffer mode accidentally kept using Z for depth tests, overlapping
polygons could sort differently from hardware whenever a game selects
`SWAP_BUFFERS.Bit1`. That kind of mismatch can show up as flickering or
incorrectly layered 3D title-screen geometry.

### Coverage added

`test_w_buffering_uses_w_for_depth_ordering` renders two overlapping polygons
whose Z values and W values disagree about which polygon is closer. With
W-buffering enabled, the polygon with smaller W must win even though its Z
value is farther.

The existing implementation already matched this behavior; this change locks
it down as part of the visual-conformance audit.

Verification:

```sh
cargo test -p nds-core test_w_buffering_uses_w_for_depth_ordering --release
cargo test --workspace --release
```

Result:

- W-buffer depth-order focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 563 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: fog follows W-buffered depth

Status: **Added focused 3D fog/depth conformance coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from GBATEK's fog notes and `SWAP_BUFFERS.Bit1`: fog depth follows the active
depth-buffer mode, so W-buffered frames must use W-derived depth for fog-table
lookup.

### Why this matters

Fog is applied after rasterization from the per-pixel attribute/depth state. If
the renderer sorted pixels by W but computed fog density from Z, scenes using
W-buffer mode could have correct polygon ordering but incorrect fog intensity.
That kind of mismatch would show as washed-out or missing fog on otherwise
stable title-screen geometry.

### Coverage added

`test_fog_uses_w_buffered_depth_when_enabled` renders a white fog-enabled
polygon with near Z but farther W while W-buffering is enabled. The fog table is
set so near Z would keep the pixel white, while W-derived depth turns it black.
The rendered framebuffer color proves the post-effect uses the W-buffered depth
stored by rasterization.

The existing implementation already matched this behavior; this change locks
it down as part of the visual-conformance audit.

Verification:

```sh
cargo test -p nds-core test_fog_uses_w_buffered_depth_when_enabled --release
cargo test --workspace --release
```

Result:

- W-buffer fog focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 564 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: texture coordinates are perspective-corrected

Status: **Added focused 3D texture-interpolation conformance coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from the DS raster path requirement that texture coordinates use the post-
projection W value for perspective-correct sampling, while vertex color remains
screen-linear.

### Why this matters

Commercial DS models frequently put title logos, character art, and UI panels
on textured polygons. If S/T coordinates were interpolated affinely in screen
space, textured surfaces with changing W would visibly swim or choose the wrong
texels even when polygon positions and vertex colors looked stable.

### Coverage added

`test_texture_coordinates_are_perspective_corrected` rasterizes one textured
scanline with endpoints whose W values differ. The midpoint is constructed so
affine interpolation would sample texel 4, while perspective-correct
interpolation samples texel 2. The test marks those texels with different
direct-color values and asserts the framebuffer receives the perspective-
correct texel.

The existing implementation already matched this behavior; this change locks
it down as part of the visual-conformance audit.

Verification:

```sh
cargo test -p nds-core test_texture_coordinates_are_perspective_corrected --release
cargo test --workspace --release
```

Result:

- Perspective-correct texture focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 565 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: repeat+flip texture coordinates reach raster sampling

Status: **Added focused 3D texture-coordinate addressing coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from GBATEK's `TEXIMAGE_PARAM` bits 16 and 18: repeat in S and flip every
second repeated S tile.

### Why this matters

Mirrored texture repeat is commonly used to tile graphics without visible hard
edges. If the standalone coordinate helper worked but the raster path failed to
carry `TEXIMAGE_PARAM` repeat/flip bits into texture sampling, commercial
models could show wrong seams, reversed panels, or clamped border texels even
when the texture data itself decoded correctly.

### Coverage added

`test_texture_repeat_flip_bits_are_applied_during_raster_sampling` rasterizes a
direct-color textured scanline with S=9 on an 8-wide texture. The marker texture
uses different colors at texel 1, texel 6, and texel 7:

- plain repeat would fetch texel 1,
- clamp would fetch texel 7,
- repeat+flip must mirror the second tile and fetch texel 6.

The rendered framebuffer color proves the full raster sampling path applies
the repeat+flip bits correctly.

The existing implementation already matched this behavior; this change locks
it down as part of the visual-conformance audit.

Verification:

```sh
cargo test -p nds-core test_texture_repeat_flip_bits_are_applied_during_raster_sampling --release
cargo test --workspace --release
```

Result:

- Texture repeat+flip raster focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 566 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: same polygon ID rejection is translucent-only

Status: **Added focused 3D alpha-blending/polygon-ID conformance coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from GBATEK's alpha-blending and polygon-ID notes: translucent polygon pixels
are rejected only after a previous translucent write with the same polygon ID.
An opaque base pixel with the same polygon ID must not suppress a later
translucent overlay.

### Why this matters

Commercial scenes can reuse polygon IDs across opaque and translucent geometry
for related model parts, masks, or effects. If the renderer rejected a
translucent pixel merely because the framebuffer's current polygon ID matched,
valid overlays would disappear. The hardware behavior needs a separate
"previous translucent ID" state, not just the ordinary polygon ID buffer.

### Coverage added

`test_same_id_translucent_can_blend_over_opaque_pixel` renders an opaque blue
triangle with polygon ID 7, then a nearer translucent red triangle with the
same ID. With alpha blending enabled, the red triangle must blend over the blue
base. This catches implementations that use the ordinary `id_buffer` for the
same-ID translucent rejection instead of tracking only prior translucent
writes.

The existing implementation already matched this behavior; this change locks
it down as part of the visual-conformance audit.

Verification:

```sh
cargo test -p nds-core test_same_id_translucent_can_blend_over_opaque_pixel --release
cargo test --workspace --release
```

Result:

- Same-ID opaque/translucent focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 567 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: shininess table controls specular intensity

Status: **Added focused 3D lighting/material conformance coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from GBATEK's `SPE_EMI.Bit15` and `SHININESS` notes: when the shininess table is
enabled, the raw specular reflection level is replaced by the table value.

### Why this matters

Specular highlights are a visible part of many commercial 3D models. If
`SPE_EMI.Bit15` were treated as only a stored flag, or if the `SHININESS` table
were loaded but not used during `NORMAL`, highlight intensity would be wrong
even when diffuse and ambient lighting looked plausible.

### Coverage added

`test_shininess_table_enabled_scales_specular_level` computes the same fully
aligned white specular highlight twice. With the table disabled, the raw
specular level produces white. With the table enabled and the matching table
entries set to zero, the same light/material/normal must produce no specular
contribution. This proves the table participates in the lighting equation, not
just command decode state.

The existing implementation already matched this behavior; this change locks
it down as part of the visual-conformance audit.

Verification:

```sh
cargo test -p nds-core test_shininess_table_enabled_scales_specular_level --release
cargo test --workspace --release
```

Result:

- Shininess-table specular focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 568 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: NORMAL recalculates current vertex color

Status: **Added focused 3D lighting command-timing coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from GBATEK's `NORMAL` and polygon-light notes: changing material, light, or
polygon light-enable bits does not by itself recolor vertices; executing
`NORMAL` recalculates the current vertex color from the active light/material
state.

### Why this matters

Commercial model command streams often bind material/light state before
emitting normals and vertices. If material writes immediately changed vertex
color, or if `NORMAL` ignored the latched `POLYGON_ATTR` light bits, lit model
surfaces could pick up stale or premature colors.

### Coverage added

`test_normal_recomputes_current_color_from_enabled_lights` drives the real GX
command path: `POLYGON_ATTR` enables light 0, `BEGIN_VTXS` latches it,
`DIF_AMB` sets red ambient without the bit15 color side effect, and
`LIGHT_COLOR` sets a white light. The test asserts the previous current color
survives until `NORMAL`, then asserts `NORMAL` recomputes the current vertex
color to red from the enabled light/material state.

The existing implementation already matched this behavior; this change locks
it down as part of the visual-conformance audit.

Verification:

```sh
cargo test -p nds-core test_normal_recomputes_current_color_from_enabled_lights --release
cargo test --workspace --release
```

Result:

- NORMAL lighting command-timing focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 569 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: clipping interpolates texture coordinates

Status: **Added focused 3D clipping/attribute conformance coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from the homogeneous clipping requirement: when an edge crosses a clip plane,
the inserted intersection vertex must carry interpolated per-vertex attributes,
including texture coordinates.

### Why this matters

Textured commercial models often cross the view frustum at screen edges. If
clipping preserved endpoint S/T values instead of interpolating them, textures
would visibly jump or smear along clipped polygon edges even if unclipped
polygons sampled correctly.

### Coverage added

`test_texcoord_interpolation_along_clipped_edge` clips a triangle against the
near plane with one outside vertex carrying distinct S/T coordinates. The
generated near-plane intersection must contain the halfway interpolated
texture coordinates `[32, 64]`, proving that clipping updates texture
attributes alongside position and color.

The existing implementation already matched this behavior; this change locks
it down as part of the visual-conformance audit.

Verification:

```sh
cargo test -p nds-core test_texcoord_interpolation_along_clipped_edge --release
cargo test --workspace --release
```

Result:

- Clipped-edge texture-coordinate focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 570 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: texture transforms through GX command path

Status: **Added focused command-path texture-transform coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from ndsdoc/GBATEK's texture-coordinate transform modes and command ordering:
mode 2 is evaluated when `NORMAL` executes, while mode 3 is evaluated when each
`VTX_*` command executes.

### Why this matters

The earlier helper-level tests proved the transform math in `VertexState`, but
commercial display lists do not call helper APIs directly. They interleave
`MTX_MODE`, `MTX_LOAD_*`, `TEXIMAGE_PARAM`, `TEXCOORD`, `NORMAL`, and `VTX_*`
commands through the geometry engine. A bug in command sequencing would produce
wrong texture coordinates even if the helper functions were correct.

### Coverage added

`test_texcoord_transform_mode_2_through_gx_command_path` loads a texture
matrix through GX matrix commands, selects transform mode 2 with
`TEXIMAGE_PARAM`, sets a base `TEXCOORD`, executes `NORMAL`, and then submits a
triangle. The emitted screen polygon must carry the normal-derived transformed
S/T values on every vertex.

`test_texcoord_transform_mode_3_through_gx_command_path` performs the same
setup for mode 3, then verifies that the first submitted `VTX_16` position is
used as the texture-transform source when the vertex command executes.

The existing implementation already matched these behaviors; this change locks
the command ordering down as part of the visual-conformance audit.

Verification:

```sh
cargo test -p nds-core texcoord_transform_mode_ --release
cargo test --workspace --release
```

Result:

- Texture-transform focused tests: `9 passed; 0 failed`.
- Workspace release tests: `nds-core 572 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: 4x4 compressed texture interpolation modes

Status: **Added focused compressed-texture conformance coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from the NDS 4x4 compressed texture mode table: mode 1 derives texel index 2
as the even average of palette colors 0 and 1 while index 3 is transparent;
mode 3 derives indices 2 and 3 from 5:3 and 3:5 weighted averages.

### Why this matters

Commercial 3D scenes often use 4x4 compressed textures for larger models and
background elements. The previous tests covered explicit-color mode 2, mode 1
transparency, and the slot-2 parameter-address quirk, but they did not prove
the interpolated color formulas. A wrong formula here would not usually break
boot, but it can tint or band textured polygons in title scenes.

### Coverage added

`test_4x4_compressed_mode_1_interpolates_index_2_evenly` verifies that mode 1
texel index 2 resolves to the per-channel average of colors 0 and 1.

`test_4x4_compressed_mode_3_uses_five_three_weighted_colors` verifies that
mode 3 derives texel index 2 from 5:3 weights and texel index 3 from 3:5
weights.

The existing implementation already matched these formulas; this change locks
them down as part of the texture-conformance sweep.

Verification:

```sh
cargo test -p nds-core 4x4_compressed --release
cargo test --workspace --release
```

Result:

- 4x4 compressed texture focused tests: `5 passed; 0 failed`.
- Workspace release tests: `nds-core 574 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: LIGHT_VECTOR uses the vector matrix

Status: **Added focused lighting command-path conformance coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from the NDS geometry/lighting command rule that `LIGHT_VECTOR` transforms the
raw 10-bit light vector by the current vector matrix when the command executes.

### Why this matters

Lit commercial models commonly update the position/vector matrix stack before
submitting light vectors and normals. Helper-level lighting tests verified
unpacking and specular behavior, but they did not prove that the real GX
command path feeds `LIGHT_VECTOR` through the active vector matrix. If that
dispatch path ignored the matrix, rotating or scaled model-light setups could
shade with stale directions even while vertex positions were otherwise correct.

### Coverage added

`test_light_vector_uses_current_vector_matrix_through_gx_command_path` loads a
position/vector matrix through GX matrix commands, dispatches `LIGHT_VECTOR`,
and verifies both the transformed light direction and the derived half-vector
stored in the lighting unit.

The existing implementation already matched this behavior; this change locks
the command-path state dependency down as part of the lighting-conformance
sweep.

Verification:

```sh
cargo test -p nds-core test_light_vector_uses_current_vector_matrix_through_gx_command_path --release
cargo test --workspace --release
```

Result:

- LIGHT_VECTOR command-path focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 575 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: MTX_SCALE does not affect light-vector transforms

Status: **Added focused position/vector matrix command-path coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from the NDS matrix-stack rule that `MTX_SCALE` in position/vector mode updates
the position matrix only; the vector matrix used for normals and light vectors
is intentionally not scaled.

### Why this matters

Commercial games often scale model transforms while keeping lighting in a
direction-only vector space. If `MTX_SCALE` also scaled the vector matrix, later
`LIGHT_VECTOR` commands would store scaled light directions and produce wrong
diffuse/specular intensity on scaled models.

### Coverage added

`test_light_vector_ignores_pos_vector_mtx_scale_command` drives the real GX
command path: select position/vector mode, issue `MTX_SCALE`, then issue
`LIGHT_VECTOR`. The stored light direction must remain the raw identity-vector
result instead of being doubled by the position scale.

The existing implementation already matched this behavior through the matrix
stack; this change locks the interaction down at the lighting command boundary.

Verification:

```sh
cargo test -p nds-core light_vector_ --release
cargo test --workspace --release
```

Result:

- Light-vector focused tests: `3 passed; 0 failed`.
- Workspace release tests: `nds-core 576 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: POS_TEST command path transforms and seeds position

Status: **Added focused geometry test-command coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from the NDS `POS_TEST` command behavior: it accepts `VTX_16`-style coordinates,
transforms them by the current clip matrix, writes the four result registers,
and updates the inherited current vertex position used by following partial
`VTX_*` commands.

### Why this matters

SDK display lists and visibility probes can use `POS_TEST` in the same command
stream as regular geometry. If the command path transformed with stale matrix
state, failed to clear the test-busy bit, or did not seed the inherited vertex
position, following geometry could differ from hardware even though isolated
helper-level tests passed.

### Coverage added

`test_pos_test_uses_clip_matrix_and_seeds_last_position_through_gx_command_path`
drives real GX dispatch: select the position matrix, apply a translation,
issue `POS_TEST`, and then verify the transformed result registers, the seeded
`last_pos`, and completion of the test-busy state.

The existing implementation already matched this behavior; this change locks
the command-path state dependency down as part of the geometry-test sweep.

Verification:

```sh
cargo test -p nds-core test_pos_test_uses_clip_matrix_and_seeds_last_position_through_gx_command_path --release
cargo test --workspace --release
```

Result:

- POS_TEST command-path focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 577 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: VEC_TEST command path uses the vector matrix

Status: **Added focused geometry vector-test command coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from the NDS `VEC_TEST` command behavior: the raw 10-bit vector must be
transformed by the current vector matrix when the command executes, and the
readback registers expose the wrapped 4.12-format result.

### Why this matters

Commercial SDK code can issue `VEC_TEST` while matrix state is changing. Helper
coverage proved the formatting helper and mode guard, but not that real GX
dispatch used the active vector matrix and completed the test-busy state. A
dispatch mismatch here would make geometry tests and following matrix readback
behave differently from hardware.

### Coverage added

`test_vec_test_uses_vector_matrix_through_gx_command_path` loads a scaled
position/vector matrix through GX commands, dispatches `VEC_TEST`, and verifies
the wrapped vector result plus test-busy completion.

The existing implementation already matched this behavior; this change locks
the command-path dependency down next to the `POS_TEST` coverage.

Verification:

```sh
cargo test -p nds-core test_vec_test_uses_vector_matrix_through_gx_command_path --release
cargo test --workspace --release
```

Result:

- VEC_TEST command-path focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 578 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: BOX_TEST command path uses the clip matrix

Status: **Added focused geometry box-test command coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from the NDS `BOX_TEST` command behavior: the packed origin/size parameters
describe a box in model coordinates, and the geometry engine tests that box
against the current clip matrix before exposing the visible bit through
`GXSTAT`.

### Why this matters

Games can use `BOX_TEST` as a lightweight visibility probe before submitting
more expensive geometry. Existing tests covered the frustum-helper math and the
top-level visible bit, but not the real GX command path with live matrix state.
If the command ignored the current clip matrix, failed to unpack the dimensions
correctly, or left `test_busy` set, visibility decisions and subsequent matrix
readback would diverge from hardware.

### Coverage added

`test_box_test_uses_clip_matrix_through_gx_command_path` dispatches an inside
box through the identity matrix, verifies the visible result, then applies a
position translation through GX commands and dispatches the same box again. The
second probe must be rejected after clip transformation. Both paths also verify
that command completion clears the test-busy state.

The existing implementation already matched this behavior; this change closes
the remaining command-path gap beside the `POS_TEST` and `VEC_TEST` coverage.

Verification:

```sh
cargo test -p nds-core test_box_test_uses_clip_matrix_through_gx_command_path --release
cargo test --workspace --release
```

Result:

- BOX_TEST command-path focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 579 passed; nds-frontend 5 passed`.

### Follow-up: BOX_TEST rejects boxes enclosing the whole view volume

Status: **Fixed BOX_TEST face-clipping conformance edge case**

Direct reference-emulator implementation use for this fix: **0**. This came
from re-checking the documented `BOX_TEST` semantics against the local helper:
the command clips the cuboid faces against the view volume. If the view volume
is fully inside the box, no box face intersects the view volume, so the
hardware-visible result is false.

### Symptom / gap

`box_intersects_view_volume()` previously treated the "box encloses the whole
view volume" case as visible. That is a reasonable generic frustum-intersection
answer, but it is not the NDS `BOX_TEST` answer because `BOX_TEST` is a
face-clipping probe rather than a general solid-volume intersection test.

Before:

```text
box:     [-2..+2] in X/Y/Z
frustum: [-1..+1]
local BOX_TEST result: true
hardware-facing result: false
```

### Fix

- Kept the fast reject when all box corners are outside any one frustum plane.
- Kept the positive result when at least one cuboid face clips into the view
  volume.
- Changed the final no-face-intersection case from visible to not visible.
- Updated the enclosing-view-volume regression from expecting true to expecting
  false.

Why this matters:

Games can use `BOX_TEST` to decide whether to submit object geometry. Returning
true for an enclosing box can make CPU-side culling logic disagree with DS
hardware in edge cases around very large bounds or camera-inside-volume tests.

Verification:

```sh
cargo test -p nds-core box_test --release
```

Result:

- BOX_TEST-focused release tests: `5 passed; 0 failed`.

## 2026-06-06 coverage: anti-aliasing preserves same-polygon interiors

Status: **Added focused anti-aliasing conformance coverage and refreshed the 3D carry-over list**

Direct reference-emulator implementation use for this check: **0**. This came
from the renderer's own anti-aliasing model and the hardware-visible invariant
that anti-aliasing should only soften exposed polygon edges, not pixels whose
four cardinal neighbors belong to the same polygon.

### Why this matters

The current AA implementation is still approximate because it does not store
true per-pixel coverage during scan conversion. That makes it important to
guard the invariants it does claim to support. If same-polygon interior pixels
were softened, large filled polygons could become visibly washed out whenever
AA is enabled, even away from silhouettes.

### Coverage added

`test_antialias_keeps_same_polygon_interior_pixels_opaque` seeds a center pixel
and its four neighbors with the same polygon ID, depth, edge eligibility, and
opaque alpha, then runs the post-effect pass with AA enabled. The center pixel
must keep its original color and alpha instead of blending toward the rear
plane.

The existing implementation already matched this behavior; this test narrows
the remaining AA risk to exact coverage weights and cross-edge neighbor choice.

### Carry-over checklist update

`debug/phase9_carryover.md` was stale: it still described several 3D features
as stubs/no-effect even though they now have implementations and focused tests.
The 3D section now records the current status for format-5 textures, AA,
toon/highlight, shadow mode, W-buffering, display capture, and geometry test
commands. AA remains explicitly marked approximate rather than complete.

Verification:

```sh
cargo test -p nds-core test_antialias_keeps_same_polygon_interior_pixels_opaque --release
cargo test --workspace --release
```

Result:

- AA same-polygon interior focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 580 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: anti-aliasing respects depth ordering

Status: **Added focused anti-aliasing depth-order coverage**

Direct reference-emulator implementation use for this check: **0**. This came
from the same silhouette rule used by edge marking: a different polygon ID only
exposes a visible edge for the center pixel when the center pixel is closer
than that neighbor.

### Why this matters

The AA implementation is still an approximation, so the depth predicate is one
of the important guardrails that keeps it from softening hidden or covered
edges. If AA blended against a different-ID neighbor that was actually in
front, overlapping polygons could get softened in the wrong direction, causing
haloing or washed borders during layered 3D scenes.

### Coverage added

`test_antialias_requires_center_closer_than_neighbor` seeds a center pixel and
its neighbors directly in the post-effect buffers. With a different-ID neighbor
that is closer, AA must leave the center pixel unchanged. With the same
different-ID neighbor farther away, AA must soften the center pixel toward the
rear plane and lower its alpha to the current approximate coverage value.

The existing implementation already matched this behavior; this test locks down
the AA depth predicate beside the same-polygon interior invariant.

Verification:

```sh
cargo test -p nds-core test_antialias_requires_center_closer_than_neighbor --release
cargo test --workspace --release
```

Result:

- AA depth-order focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 581 passed; nds-frontend 5 passed`.

## 2026-06-06 fix: anti-aliasing uses the actual rear-plane pixel color

Status: **Fixed rear-plane color selection for anti-aliasing**

Direct reference-emulator implementation use for this check: **0**. This came
from the DS rear-plane model: the rear plane can be either the scalar
`CLEAR_COLOR` value or the bitmap clear image selected by `DISP3DCNT.Bit14`.
Anti-aliasing should soften exposed polygon edges toward the rear-plane pixel
under that edge, not always toward `CLEAR_COLOR`.

### Symptom / gap

The AA post-pass used `rast.clear_color & 0x7FFF` as the blend target for every
softened pixel. That is correct only when the frame was cleared from
`CLEAR_COLOR`. When the rear plane comes from texture slots 2/3, the true rear
color can vary per pixel. In that mode, softened silhouettes could pick up the
wrong background color, causing visible halos around 3D edges over rear bitmap
scenes.

### Fix

Added `Rasterizer::rear_color_buffer`, a 256x192 snapshot of the frame's rear
color plane:

- register clears fill it from `CLEAR_COLOR`;
- rear-bitmap clears fill it from the bitmap color texels;
- the AA post-pass blends each softened pixel against
  `rear_color_buffer[idx] & 0x7FFF`.

The field has a serde default so older save states that do not contain it can
still deserialize.

### Coverage added

`test_antialias_blends_against_rear_plane_pixel_color` sets `CLEAR_COLOR` to
white but seeds the rear-plane pixel snapshot to green. AA must blend a blue
silhouette pixel toward green, proving the post-pass no longer relies only on
the scalar clear color.

Verification:

```sh
cargo test -p nds-core test_antialias_blends_against_rear_plane_pixel_color --release
cargo test --workspace --release
```

Result:

- AA rear-plane color focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 582 passed; nds-frontend 5 passed`.

## 2026-06-06 coverage: rear-plane color buffer clear paths

Status: **Added focused rear-plane buffer coverage**

Direct reference-emulator implementation use for this check: **0**. This
continues the AA rear-plane fix by guarding the source data that anti-aliasing
now consumes.

### Why this matters

`rear_color_buffer` is only useful if every frame clear path populates it with
the same rear color that the hardware would expose behind 3D pixels. A later
change to register clear or rear-bitmap clear could otherwise leave AA blending
against stale or zero data even though the framebuffer itself looked correct
before post-processing.

### Coverage added

- `test_clear_color_initializes_rear_color_buffer` verifies scalar
  `CLEAR_COLOR` clears fill the rear-color snapshot, including the alpha bit
  behavior used by the 3D compositor.
- `test_rear_bitmap_clear_uses_texture_slots_and_scroll` now also asserts that
  rear-bitmap color texels populate `rear_color_buffer` for both opaque and
  transparent rear pixels.

Verification:

```sh
cargo test -p nds-core rear_color_buffer --release
cargo test -p nds-core test_rear_bitmap_clear_uses_texture_slots_and_scroll --release
cargo test --workspace --release
```

Result:

- Rear-color buffer focused test: `1 passed; 0 failed`.
- Rear-bitmap clear focused test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 583 passed; nds-frontend 5 passed`.

## 2026-06-06 runtime check: HeartGold title capture after AA/rear-plane fixes

Status: **Captured current target-game frame with rebuilt release frontend**

Direct reference-emulator implementation use for this check: **0**. This is a
local runtime verification against the current worktree and the user's
HeartGold ROM.

### Why this matters

The recent work changed post-effect behavior and added a serialized rear-plane
color buffer. Unit tests cover the individual invariants, but the original
HeartGold problem was visual: black screens, a large screen gap, random
polygons, and title-scene artifacts. A fresh target-game capture gives a
coarse but important regression check that the current renderer still produces
a coherent title frame.

### Commands run

```sh
cargo build --release -p nds-frontend
./target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 4320 --capture-ppm /private/tmp/heartgold-3d-current.ppm
sips -s format png /private/tmp/heartgold-3d-current.ppm --out /private/tmp/heartgold-3d-current.png
```

### Result

- Release frontend rebuilt successfully.
- Frame 4320 captured to `/private/tmp/heartgold-3d-current.ppm`.
- The PPM reports `256 x 384`, matching compact stacked DS screens with no
  extra gap.
- Visual inspection of `/private/tmp/heartgold-3d-current.png` shows a coherent
  title frame: Ho-Oh scene on the top screen, Pokemon HeartGold logo on the
  bottom screen, no black screen, and no random-polygon flashing in this
  sampled frame.

Remaining risk: this is still a single sampled frame. Exact AA coverage,
post-effect ordering, and subtle texture/edge correctness still need more
image-level test ROMs or frame-sequence comparisons before claiming full 3D
visual conformance.

## 2026-06-06 docs: rasterization concept aligned with current implementation

Status: **Updated concept documentation for current 3D raster state**

Direct reference-emulator implementation use for this check: **0**. This was a
documentation correction driven by the current code and the recent conformance
work.

### Why this matters

`docs/concepts/rasterization.md` still mixed older phase-plan language with
current implementation details. It described display capture as formerly
stubbed, implied toon/highlight was a separate post-pass, and described AA only
as ideal hardware coverage. That could point future debugging at already-solved
work or hide the actual remaining risk.

### Changes made

- Reframed the doc as the current raster model, not a Phase 7 plan.
- Documented that the emulator's AA path is an approximate edge-only post-pass
  over polygon/depth/rear-plane buffers, with exact coverage still remaining.
- Documented toon/highlight as a per-polygon texture/color combine behavior for
  `POLYGON_ATTR.mode = 2`, not a separate framebuffer post-pass.
- Updated display capture wording to describe the covered implementation.
- Replaced the old module plan with the current implementation shape:
  `engine.rs`, `triangle.rs`, `texture.rs`, `raster/mod.rs`, `postfx.rs`,
  `gpu2d/compositor.rs`, and `lib.rs`.

Verification:

```sh
grep -n "stubbed\|will need\|Phase 7 needs\|Phase 7's job\|Current implementation" docs/concepts/rasterization.md
```

Result:

- Only the intentional `Current implementation shape` heading remains.
- No code tests were run for this documentation-only change.

## 2026-06-06 fix: fog color mode preserves zero-alpha transparency

Status: **Fixed fog color+alpha framebuffer alpha-bit handling**

Direct reference-emulator implementation use for this check: **0**. This came
from auditing the local fog post-effect path and the renderer's own invariant
that framebuffer bit 15 follows the 3D alpha buffer.

### Symptom / gap

`apply_fog()` correctly computed the fogged alpha value and cleared framebuffer
bit 15 when the result became zero. However, in normal color+alpha fog mode, it
then recomputed the RGB channels and unconditionally wrote bit 15 back into the
framebuffer. That meant a pixel whose fog alpha faded to zero could still look
opaque to the later 2D compositor.

### Fix

The color+alpha fog path now writes bit 15 only when the computed fogged alpha
is nonzero:

- alpha buffer remains the source of truth for 3D alpha;
- color channels still fog toward `FOG_COLOR`;
- framebuffer bit 15 no longer gets restored after alpha reaches zero.

### Coverage added

`test_fog_color_mode_preserves_zero_alpha_transparency` seeds a fog-enabled
opaque white pixel, applies full-density color+alpha fog with `FOG_COLOR` alpha
0 beyond the first-boundary alpha quirk, and verifies:

- `alpha_buffer` becomes 0;
- framebuffer bit 15 stays clear;
- RGB still fogs toward the configured fog color.

### Test adjustment

The first workspace run exposed a stale assumption in
`test_edge_marking_color_is_not_fogged`: it used full-density fog with fog
alpha 0 while expecting the polygon to remain visible for edge marking. After
the fix, that setup correctly makes the pixel transparent before edge marking.
The test now uses opaque black fog so it continues to verify the intended
ordering: fog darkens polygon color first, then edge marking replaces that
fogged color with the edge color.

Verification:

```sh
cargo test -p nds-core test_fog_color_mode_preserves_zero_alpha_transparency --release
cargo test -p nds-core test_edge_marking_color_is_not_fogged --release
cargo test --workspace --release
```

Result:

- Fog color+alpha focused test: `1 passed; 0 failed`.
- Edge-marking/fog ordering focused test: `1 passed; 0 failed`.
- Workspace release tests: initial run failed on the stale edge-marking test;
  after the test setup correction, `nds-core 584 passed; nds-frontend 5 passed`.

## 2026-06-06 fix: AA uses scanline coverage hints when available

Status: **Improved anti-aliasing coverage model without replacing the post-pass**

Direct reference-emulator implementation use for this check: **0**. This came
from auditing the local rasterizer/post-effect path and tightening the current
approximation around data the scanline filler already has.

### Symptom / gap

The AA pass detected opaque silhouettes from polygon ID/depth neighbors, but
every softened pixel used the same fixed 50% blend against the rear plane.
That preserved major interactions but made all polygon edges equally soft,
which is visibly too blunt for shallow or subpixel-aligned edges.

### Fix

The rasterizer now keeps an internal `aa_coverage_buffer`:

- frame clear and rear-bitmap clear reset the buffer;
- opaque scanline pixels on left/right triangle edges get a fractional
  coverage hint derived from the edge position inside the pixel;
- the AA post-pass uses that coverage value when present;
- pixels without a coverage hint keep the old conservative 50% fallback.

This is still not full hardware AA. It improves the left/right edge path while
leaving top/bottom-edge coverage and exact cross-edge-neighbor selection as
known conformance work.

### Coverage added

- `test_scanline_pixel_coverage_tracks_fractional_edges` verifies the coverage
  estimator records half coverage at exact edge-centered pixels and full
  coverage for interior pixels.
- `test_antialias_uses_rasterized_coverage_hint` verifies the post-pass prefers
  a coverage hint over the fixed fallback and writes the matching alpha value.
- `test_clear_color_initializes_rear_color_buffer` now also verifies clear
  resets stale AA coverage hints.

Verification:

```sh
cargo test -p nds-core test_scanline_pixel_coverage_tracks_fractional_edges --release
cargo test -p nds-core test_antialias_uses_rasterized_coverage_hint --release
cargo test -p nds-core test_clear_color_initializes_rear_color_buffer --release
cargo test --workspace --release
```

Result:

- Focused AA coverage estimator test: `1 passed; 0 failed`.
- Focused AA post-pass coverage-hint test: `1 passed; 0 failed`.
- Focused clear/reset regression test: `1 passed; 0 failed`.
- Workspace release tests: `nds-core 586 passed; nds-frontend 5 passed`.

## 2026-06-06 fix: AA blends visible internal edges toward neighbor color

Status: **Improved anti-aliasing blend target selection**

Direct reference-emulator implementation use for this check: **0**. This came
from reviewing the local AA post-pass against the renderer's own stated
coverage model: edge pixels should soften toward the color across the exposed
edge when that color is visible.

### Symptom / gap

After adding AA coverage hints, the post-pass still used the rear-plane color
as the blend target for every exposed edge. That is reasonable for silhouettes
against background/rear-plane pixels, but it is wrong for visible internal
polygon boundaries: a blue edge next to a green polygon should soften toward
green, not toward the clear color behind both polygons.

### Fix

The AA pass now selects a blend target while checking exposed neighbors:

- if the exposed neighbor has a visible framebuffer pixel, blend toward that
  neighbor's color;
- if the exposed neighbor is transparent/background, blend toward the current
  pixel's rear-plane color;
- out-of-bounds screen edges also use the current rear-plane fallback;
- the existing coverage hint/fixed-fallback alpha decision is unchanged.

The first focused run intentionally exposed a fallback nuance:
`test_antialias_blends_against_rear_plane_pixel_color` failed when transparent
neighbors used the neighbor's rear-plane color instead of the current pixel's
rear-plane color. The fallback now uses the current pixel, which preserves the
uncovered-fragment model for background silhouettes.

### Coverage added / adjusted

- `test_antialias_blends_against_visible_neighbor_color` verifies internal
  edges blend toward a farther visible neighbor pixel.
- `test_antialias_requires_center_closer_than_neighbor` now expects the
  farther visible neighbor color rather than the rear plane.
- Existing rear-plane and depth-gating AA tests continue to pass.

Verification:

```sh
cargo test -p nds-core antialias --release
```

Result:

- AA-focused release tests: `13 passed; 0 failed`.

## 2026-06-06 fix: AA records vertical row coverage for flat top/bottom edges

Status: **Improved anti-aliasing coverage hints for horizontal polygon edges**

Direct reference-emulator implementation use for this check: **0**. This came
from auditing the local scanline rasterizer after the previous AA fixes showed
that coverage was still only derived from the horizontal span.

### Symptom / gap

The AA coverage buffer only used `scanline_pixel_coverage(x, left_x, right_x)`.
That captures left/right fractional edge coverage, but a flat top or bottom
edge can have full horizontal coverage across the row while still covering only
part of the pixel vertically. Those pixels fell back to the fixed 50% AA blend
instead of recording a coverage value from scan conversion.

### Fix

`Vert` now preserves the original fixed-point screen Y as `y_fp`. During
triangle scan conversion, each rasterized row computes a vertical coverage hint
from the triangle's fixed-point top/bottom bounds and combines it with the
existing horizontal span coverage. Opaque pixels store the minimum of the two
coverage values in `aa_coverage_buffer`; translucent and fully covered pixels
continue to clear the hint.

This improves flat top/bottom AA behavior while keeping the existing
post-pass fallback for cases where no coverage hint is available.

### Coverage added

- `test_vertical_pixel_coverage_tracks_flat_top_bottom_edges` verifies the
  vertical coverage helper reports half coverage on exact flat boundaries and
  full coverage in the interior row.
- `test_flat_top_triangle_records_vertical_aa_coverage` verifies a normal
  flat-top polygon records vertical AA coverage during `rasterize_polygon`.

Verification:

```sh
cargo test -p nds-core coverage --release
```

Result:

- Coverage-focused release tests: `4 passed; 0 failed`.

## 2026-06-06 fix: AA uses rasterized edge-direction hints

Status: **Improved anti-aliasing neighbor selection for ambiguous edges**

Direct reference-emulator implementation use for this check: **0**. This came
from auditing the local AA post-pass after coverage hints were added: the
post-pass still fell back to a fixed neighbor scan order when multiple exposed
neighbors existed.

### Symptom / gap

The AA pass could now choose a better coverage value, but it did not know which
side of the pixel caused that partial coverage. At corners or near multiple
farther neighbors, it tried left, right, up, then down. That can pick the wrong
blend color even when scan conversion knows the coverage-limiting edge was
vertical or horizontal.

### Fix

The rasterizer now keeps an `aa_edge_hint_buffer` alongside
`aa_coverage_buffer`:

- horizontal span coverage records `LEFT` or `RIGHT` when that side limited
  the pixel coverage;
- vertical row coverage records `UP` or `DOWN` when a flat top/bottom edge
  limited the row coverage;
- the primary hint comes from the smaller coverage value;
- the AA post-pass tries the hinted neighbor first, then falls back to the old
  conservative scan order;
- frame clear and rear-bitmap clear reset the hint buffer.

This does not fully solve corner AA yet because a pixel can be limited by more
than one edge and the current buffer stores one primary hint. It does remove
the scan-order artifact for the common case where one edge clearly determines
coverage.

### Coverage added / adjusted

- `test_antialias_prefers_rasterized_edge_direction_hint` creates competing
  farther left/up neighbors and verifies the `UP` hint chooses the upper
  neighbor before fallback scan order.
- `test_scanline_pixel_coverage_tracks_fractional_edges` now verifies left and
  right coverage directions.
- `test_vertical_pixel_coverage_tracks_flat_top_bottom_edges` now verifies up
  and down coverage directions.
- `test_flat_top_triangle_records_vertical_aa_coverage` now also verifies the
  stored `UP` hint.
- `test_clear_color_initializes_rear_color_buffer` now verifies clear resets
  stale AA edge hints.

Verification:

```sh
cargo test -p nds-core antialias --release
cargo test -p nds-core coverage --release
cargo test -p nds-core clear_color_initializes_rear_color_buffer --release
```

Result:

- AA-focused release tests: `14 passed; 0 failed`.
- Coverage-focused release tests: `4 passed; 0 failed`.
- Clear/reset focused release test: `1 passed; 0 failed`.

## 2026-06-06 fix: AA preserves multi-edge corner hints

Status: **Improved anti-aliasing corner-neighbor selection**

Direct reference-emulator implementation use for this check: **0**. This came
from continuing the local AA edge-hint audit: after adding a direction hint,
corner pixels could still lose one edge when horizontal and vertical coverage
were tied.

### Symptom / gap

`aa_edge_hint_buffer` stored enum-like direction values and scan conversion
selected one primary hint. For a corner pixel where horizontal and vertical
coverage were equally limiting, one side was discarded. The post-pass could
then fall back to an unrelated exposed neighbor before trying the other true
edge direction.

### Fix

AA edge hints are now bitmasks:

- `AA_EDGE_LEFT`, `RIGHT`, `UP`, and `DOWN` are independent bits;
- scan conversion stores the smaller coverage side as before;
- if horizontal and vertical coverage tie, their direction hints are ORed
  together;
- the AA post-pass tries all hinted directions before the fallback scan order.

The remaining conformance risk is now the coverage model itself: values are
still derived from the emulator's scanline span/row approximation rather than
validated hardware edge equations.

### Coverage added / adjusted

- `test_equal_xy_coverage_preserves_corner_edge_hints` verifies tied horizontal
  and vertical coverage stores a combined left+up hint.
- `test_antialias_multi_edge_hint_ignores_unhinted_neighbors` verifies a
  multi-edge hint ignores an unrelated exposed left neighbor before trying the
  hinted right/up neighbors.
- Existing direction helper tests now run with bitmask constants.

Verification:

```sh
cargo test -p nds-core antialias --release
cargo test -p nds-core coverage --release
cargo test -p nds-core equal_xy_coverage --release
```

Result:

- AA-focused release tests: `15 passed; 0 failed`.
- Coverage-focused release tests: `5 passed; 0 failed`.
- Equal-coverage focused test: `1 passed; 0 failed`.

## 2026-06-06 fix: AA coverage uses clipped triangle area

Status: **Replaced runtime span/row AA coverage with per-pixel triangle area**

Direct reference-emulator implementation use for this check: **0**. This came
from auditing the remaining AA conformance gap after edge-direction hints were
added: coverage strength was still based on the minimum of horizontal span
coverage and vertical row coverage, not the actual triangle area inside the
pixel.

### Symptom / gap

The previous AA coverage approximation handled flat top/bottom edges and
left/right edges, but it could not model diagonal edges and corner pixels as a
real clipped triangle area. A pixel centered exactly on a right-triangle corner
should have roughly one-quarter coverage; the old model could only infer that
from separate span/row heuristics.

### Fix

When AA is enabled, `rasterize_triangle()` now passes the triangle's fixed-point
screen coordinates into `rasterize_scanline()`. For each written pixel, the AA
path clips the pixel square against the triangle's three edge half-planes and
computes the clipped polygon area. That area is converted to the 0..31 coverage
value consumed by the post-pass. The same helper also derives an edge-direction
bitmask from adjacent pixel-center tests, so corner pixels keep the
multi-direction behavior from the previous fix.

The old span/row helpers remain as fallback/test scaffolding for manual
scanline tests where no triangle geometry is supplied.

### Coverage added

- `test_triangle_pixel_coverage_uses_clipped_area` verifies a pixel centered on
  a two-pixel right-triangle corner records one-quarter coverage (`8/31`) and
  left+up edge hints.
- `test_area_coverage_drives_rasterized_aa_hint` verifies normal
  `rasterize_polygon()` writes the clipped-area coverage/hints into the AA
  buffers.

Verification:

```sh
cargo test -p nds-core coverage --release
cargo test -p nds-core antialias --release
```

Result:

- Coverage-focused release tests: `7 passed; 0 failed`.
- AA-focused release tests: `15 passed; 0 failed`.

## 2026-06-06 fix: AA transparent rear-plane exposure is alpha-only

Status: **Fixed HeartGold title-screen black diagonal stipple with AA enabled**

Direct reference-emulator implementation use for this check: **0**. This was
debugged by comparing captures from the local renderer with AA enabled versus a
hidden diagnostic run that skipped only the AA post-pass.

### Symptom

Frame 4320 of `Pokemon-HeartGoldVersionUSA.nds` had become coherent enough to
show the title scene, but the top screen still showed black diagonal/stipple
artifacts across the sky and ground. Capturing the same frame with the AA
post-pass disabled removed those artifacts while leaving the base 3D scene
intact. That isolated the issue to post-AA compositing, not texture decode,
geometry, clipping, or base triangle fill.

### Cause

The AA post-pass treated every exposed transparent neighbor as a color target
and pre-blended the 3D edge pixel against the rasterizer's rear-plane color.
For HeartGold, those exposed pixels are composited over 2D layers later. The
3D rear plane is transparent there, so pre-blending to its stored color
effectively baked the wrong background into the 3D pixel before the 2D
compositor saw it. The visible result was dark/black stippling along many
AA edges.

### Fix

AA target selection now distinguishes:

- visible 3D neighbor pixels: pre-blend the edge color toward that neighbor;
- opaque rear-plane pixels: pre-blend toward the rear-plane color;
- transparent rear-plane exposure: leave the 3D color unchanged and lower only
  the 3D alpha buffer to the AA coverage value.

That lets the 2D compositor receive an antialiased 3D pixel without baking in a
wrong transparent-rear color.

### Diagnostic support

Added a hidden frontend flag for capture/debug isolation:

```sh
./target/release/nds-frontend --rom /path/to/game.nds --no-audio --debug-disable-3d-aa
```

The flag skips only the 3D AA post-pass. It does not change normal runs and was
used to confirm that the base HeartGold title render was clean before the AA
fix.

### Coverage added / adjusted

- `test_antialias_transparent_rear_plane_preserves_color_and_lowers_alpha`
  verifies transparent rear-plane exposure does not pre-blend 3D color against
  `CLEAR_COLOR`.
- Rear-plane AA tests now put the rear color on the exposed neighboring pixel,
  matching the post-pass neighborhood lookup.
- `test_sloped_quad_fan_has_continuous_interior_coverage` exhaustively checks a
  sloped split quad for simple internal fan holes; it passed, ruling out a
  basic quad fan gap as the cause of the HeartGold stipple.

Verification:

```sh
cargo test -p nds-core antialias --release
cargo test -p nds-core test_sloped_quad_fan_has_continuous_interior_coverage --release
./target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 4320 --capture-ppm /private/tmp/heartgold-aa-fixed.ppm
```

Result:

- AA-focused release tests: `16 passed; 0 failed`.
- Sloped-quad focused release test: `1 passed; 0 failed`.
- HeartGold frame 4320 with AA enabled no longer shows the black diagonal
  stipple pattern; the top and bottom title screens are coherent.

## 2026-06-06 validation: HeartGold AA title sweep and 3D-to-2D handoff

Status: **Broadened HeartGold title validation after the AA transparent-rear fix**

Direct reference-emulator implementation use for this check: **0**. This was a
local capture sweep and regression-test pass over the BG0-from-3D handoff that
GBATEK describes for 3D final output.

### Visual validation

Captured a short HeartGold title sequence with AA enabled:

```sh
./target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 5400 --capture-interval 540 --capture-dir /private/tmp/heartgold-aa-sweep
```

Representative frames inspected:

- `/private/tmp/heartgold-aa-sweep/frame-002700.png`
- `/private/tmp/heartgold-aa-sweep/frame-004320.png`
- `/private/tmp/heartgold-aa-sweep/frame-005400.png`

Result: the sampled title/animation frames are coherent, with no return of the
previous random polygon flashes or black AA stipple pattern.

### Regression added

Added `test_3d_bg0_antialias_alpha_composes_over_2d_second_target` in the 2D
compositor tests. It models the AA handoff directly: the 3D framebuffer keeps
the edge pixel's color, the 3D alpha buffer carries AA coverage, and Engine A
composes BG0-from-3D over a BG1 second target using that 3D alpha instead of
pre-baking a rear-plane color.

Verification:

```sh
cargo test -p nds-core antialias --release
cargo test -p nds-core 3d_bg0 --release
```

Result:

- AA-focused release tests: `17 passed; 0 failed`.
- BG0-from-3D focused release tests: `7 passed; 0 failed`.

## 2026-06-06 fix: visible shadows require a set stencil bit

Status: **Corrected shadow polygon stencil semantics**

Direct reference-emulator implementation use for this check: **0**. This came
from re-auditing the shadow-mode notes while working through remaining 3D
conformance risks. The earlier local implementation followed the older wording
that visible shadows render where the stencil bit is clear. The GBATEK addendum
notes the corrected behavior: the shadow mask pass sets stencil bits, and the
visible shadow pass draws where those bits are set, provided the destination
polygon ID differs.

### Symptom / risk

The old helper let visible shadow polygons draw even when no shadow mask had
been written, and skipped drawing where a mask was present. That inverts the
two-pass shadow-volume model and would make commercial shadow volumes appear in
the wrong places or disappear where the game actually prepared a mask.

### Fix

`shadow_fragment_is_hidden_or_masked()` now uses this order:

- polygon ID `0`: write the shadow stencil bit and skip color;
- nonzero visible shadow with same destination polygon ID: reject without
  consuming the mask;
- nonzero visible shadow with no stencil bit: reject;
- nonzero visible shadow with a set stencil bit and different destination ID:
  clear/consume the stencil bit and draw/blend the shadow pixel.

The same helper is still shared by filled polygons, lines, and zero-dot paths.

### Coverage adjusted

- `test_visible_shadow_draws_only_where_mask_is_set`
- `test_visible_shadow_line_draws_only_where_mask_is_set`
- `test_visible_shadow_zero_dot_draws_only_where_mask_is_set`
- `test_visible_shadow_uses_polygon_alpha_as_intensity`
- `test_visible_shadow_same_id_reject_preserves_mask`

Verification:

```sh
cargo test -p nds-core shadow --release
cargo test --workspace --release
./target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 4320 --capture-ppm /private/tmp/heartgold-shadow-fix-smoke.ppm
```

Result:

- Shadow-focused release tests: `9 passed; 0 failed`.
- Full release workspace suite: `nds-core` `597 passed; 0 failed`,
  `nds-frontend` `5 passed; 0 failed`.
- HeartGold frame 4320 smoke capture remains coherent after the shadow
  semantic change.

## 2026-06-06 fix: display capture 128x128 uses compact output stride

Status: **Corrected `DISPCAPCNT` output layout for 128-wide captures**

Direct reference-emulator implementation use for this check: **0**. This came
from auditing the remaining display-capture conformance risk against GBATEK's
capture-size notes. Source reads still use screen coordinates, but the captured
destination bitmap is packed according to the selected capture width.

### Symptom / risk

The capture writer used `(line * 256 + x) * 2` for every capture size. That is
correct for the 256-wide capture modes, but it leaves a 128-pixel gap after
each row in 128x128 capture mode. Games that capture a 128x128 image and later
sample it as a compact texture would see every row after the first at the wrong
VRAM address.

### Fix

Added `capture_output_byte_pos(width, line, x)`. The writer now uses:

- stride 128 when `DISPCAPCNT[21:20]` selects 128x128;
- stride 256 for the 256-wide modes.

Source-A framebuffer reads and source-B VRAM reads remain screen-strided at
256 pixels, matching the existing source-coordinate behavior.

### Coverage added

- `test_display_capture_128_width_uses_compact_output_stride` captures two
  128-wide rows and verifies row 1 starts immediately after row 0 at byte
  offset `0x100`, not at `0x200`.

Verification:

```sh
cargo test -p nds-core test_display_capture_128_width_uses_compact_output_stride --release
cargo test -p nds-core display_capture --release
cargo test --workspace --release
```

Result:

- Focused 128-wide capture stride test: `1 passed; 0 failed`.
- Display-capture focused release tests: `8 passed; 0 failed`.
- Full release workspace suite: `nds-core` `598 passed; 0 failed`,
  `nds-frontend` `5 passed; 0 failed`.

## 2026-06-06 coverage: edge-marked zero-dot AA is exact

Status: **Tightened the edge-marking plus anti-aliasing zero-dot regression**

Direct reference-emulator implementation use for this check: **0**. This is a
test hardening pass based on our existing model: opaque zero-dot polygons are
hidden when AA is enabled alone, but the edge-marking quirk keeps them visible
when both AA and edge marking are enabled.

### Why this mattered

The previous regression only asserted that the pixel remained visible and had
nonzero alpha. That left too much room for a false pass: the pixel could remain
visible without proving that the edge-mark post-pass selected the polygon's
edge-color group and that AA left behind the expected coverage alpha.

### Tightened fixture

- The clear plane now has a different polygon ID from the test point.
- The zero-dot polygon uses ID `8`, which maps to edge-color group `1`.
- `EDGE_COLOR[1]` is set to green.

The assertion now checks:

- framebuffer color is exactly the selected edge color;
- the alpha/visible bit is still set;
- AA coverage alpha is exactly the fallback `16`.

Verification:

```sh
cargo test -p nds-core test_edge_marking_keeps_antialiased_zero_dot_polygon_visible --release
cargo test -p nds-core edge_marking --release
```

Result:

- Focused zero-dot edge-mark/AA test: `1 passed; 0 failed`.
- Edge-marking focused release tests: `11 passed; 0 failed`.

## 2026-06-06 coverage: W-buffer equal-depth draw path

Status: **Added W-buffer integration coverage for equal-depth tolerance**

Direct reference-emulator implementation use for this check: **0**. This is a
coverage pass around the existing depth model, not a behavior change. The
helper-level `depth_test_passes()` boundary test already checked the inclusive
`+/-0x200` depth tolerance, but it did not prove that W-buffer conversion and
polygon rasterization preserved the same behavior in an actual draw.

### Coverage added

- `test_w_buffering_depth_equal_allows_later_polygon_within_tolerance`
  draws two overlapping opaque polygons in W-buffer mode. The later polygon has
  `POLYGON_ATTR` bit 14 set and is one W-depth step farther away, which expands
  to the inclusive tolerance boundary. It must overwrite the first polygon.
- `test_w_buffering_depth_equal_rejects_later_polygon_outside_tolerance`
  repeats the same setup with the later polygon two W-depth steps farther away.
  It must be rejected, leaving the first polygon visible.

Verification:

```sh
cargo test -p nds-core test_w_buffering_depth_equal --release
cargo test -p nds-core w_buffering --release
```

Result:

- Focused W-buffer equal-depth tests: `2 passed; 0 failed`.
- W-buffer focused release tests: `3 passed; 0 failed`.

## 2026-06-06 smoke: HeartGold title frame remains coherent

Status: **Captured a fresh HeartGold title frame after the latest raster coverage work**

Command:

```sh
cargo run --release -p nds-frontend -- --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 4320 --capture-ppm /private/tmp/heartgold-20260606-current.ppm
```

Result:

- Capture completed successfully and wrote
  `/private/tmp/heartgold-20260606-current.ppm`.
- Converted inspection copy:
  `/private/tmp/heartgold-20260606-current.png`.
- The frame is coherent: Ho-Oh, the title prompt, the HeartGold logo, and
  the Game Freak text are visible. The old random polygon flashing seen in the
  earlier failing screenshots did not reproduce in this capture.

## 2026-06-06 coverage: compressed texture palette base and short capture height

Status: **Added two focused commercial-visual conformance fixtures**

Direct reference-emulator implementation use for this check: **0**. Both checks
are local invariants from the emulator's implemented hardware model.

### 4x4 compressed texture palette base

The existing compressed-texture tests covered mode decoding, transparent index
behavior, interpolation, weighted colors, and slot-2 parameter-table routing,
but they all sampled with `PLTT_BASE = 0`. Added
`test_4x4_compressed_palette_base_offsets_palette_lookup`, which places a red
color at palette base 0 and a green color at `PLTT_BASE = 1`; the sample must
return green. This catches accidental base-zero compressed-texture sampling.

Verification:

```sh
cargo test -p nds-core test_4x4_compressed_palette_base_offsets_palette_lookup --release
cargo test -p nds-core compressed --release
```

Result:

- Focused compressed palette-base test: `1 passed; 0 failed`.
- Compressed-texture focused release tests: `6 passed; 0 failed`.

### Display capture 256x64 stride and height cutoff

The previous capture stride fix proved 128x128 uses compact 128-pixel rows.
Added `test_display_capture_256x64_uses_screen_stride_and_stops_at_height` to
prove the 256-wide short capture mode keeps a 256-pixel destination stride and
does not write line 64. The test also documents the current busy-bit behavior:
short captures stop writing after their height, but the capture busy bit remains
set until the visible frame ends.

Verification:

```sh
cargo test -p nds-core test_display_capture_256x64_uses_screen_stride_and_stops_at_height --release
cargo test -p nds-core display_capture --release
```

Result:

- Focused 256x64 capture test: `1 passed; 0 failed`.
- Display-capture focused release tests: `9 passed; 0 failed`.

## 2026-06-06 coverage: edge marking preserves fogged alpha

Status: **Tightened fog and edge-marking post-effect ordering coverage**

Direct reference-emulator implementation use for this check: **0**. This is a
local ordering invariant from the existing post-effect pipeline: fog runs before
edge marking, and edge marking replaces only the color with `EDGE_COLOR`.

### Why this mattered

`test_edge_marking_color_is_not_fogged` already proved that an edge-marked
pixel uses the configured edge color rather than the fog-blended polygon color.
It did not prove what happens to the alpha channel after fog has already
modified it. A regression there would keep edge colors looking plausible while
changing how the pixel composes over 2D layers or into capture targets.

### Coverage added

Added `test_edge_marking_keeps_fogged_alpha`. The fixture:

- enables fog and edge marking together;
- lets fog lower the center pixel alpha from `31` to `16`;
- creates a depth/ID edge against the neighboring pixel;
- verifies edge marking changes the color to `EDGE_COLOR`;
- verifies the alpha buffer remains the fogged value `16`.

Verification:

```sh
cargo test -p nds-core test_edge_marking_keeps_fogged_alpha --release
cargo test -p nds-core edge_marking --release
cargo test -p nds-core fog --release
```

Result:

- Focused edge/fog alpha test: `1 passed; 0 failed`.
- Edge-marking focused release tests: `12 passed; 0 failed`.
- Fog focused release tests: `19 passed; 0 failed`.

## 2026-06-06 coverage: same-ID translucent reject preserves state

Status: **Tightened translucent same-polygon-ID rejection coverage**

Direct reference-emulator implementation use for this check: **0**. This is a
coverage pass around the existing NDS translucent-ID rule: a translucent
fragment rejects only when a previous translucent fragment with the same polygon
ID already contributed to that pixel.

### Why this mattered

`test_same_id_translucent_overlap_does_not_blend_twice` already proved that the
second same-ID translucent polygon does not blend into the framebuffer again.
It did not prove that the rejected fragment leaves all per-pixel side state
alone. If a rejected fragment still changed alpha, fog, or translucent-ID state,
later fog/edge/overlap behavior could diverge even when the immediate color
looked correct.

### Coverage added

Added `test_same_id_translucent_reject_preserves_fragment_state`. The fixture:

- draws an opaque fog-enabled base;
- draws a first translucent same-ID overlay that blends once and keeps fog
  state;
- submits a second same-ID translucent polygon with a different color, different
  alpha, and fog disabled;
- verifies the second polygon does not change color, alpha, fog enable state,
  or the recorded translucent polygon ID.

Verification:

```sh
cargo test -p nds-core test_same_id_translucent_reject_preserves_fragment_state --release
cargo test -p nds-core translucent --release
```

Result:

- Focused same-ID reject state test: `1 passed; 0 failed`.
- Translucent focused release tests: `32 passed; 0 failed`.

## 2026-06-06 coverage: translucent depth update occludes later translucent fragments

Status: **Tightened translucent depth-update draw-path coverage**

Direct reference-emulator implementation use for this check: **0**. This is a
coverage pass around `POLYGON_ATTR` bit 11, which lets translucent fragments
write depth.

### Why this mattered

`test_translucent_polygon_updates_depth_with_attr_bit11` already proved that a
translucent fragment with bit 11 set writes the converted depth value. It did
not prove that the updated depth participates in subsequent translucent depth
tests. A renderer could pass the buffer-value assertion while still letting a
later behind-the-front translucent fragment blend when it should be occluded.

### Coverage added

Added `test_translucent_depth_update_occludes_later_translucent_fragment`. The
fixture:

- draws an opaque base polygon;
- draws a front translucent polygon with depth-update enabled;
- draws a second translucent polygon behind that front translucent depth but
  still in front of the opaque base;
- verifies only the first translucent overlay contributes to the framebuffer;
- verifies the depth buffer remains at the front translucent depth.

Verification:

```sh
cargo test -p nds-core test_translucent_depth_update_occludes_later_translucent_fragment --release
cargo test -p nds-core translucent --release
```

Result:

- Focused translucent depth-update occlusion test: `1 passed; 0 failed`.
- Translucent focused release tests: `33 passed; 0 failed`.

## 2026-06-06 coverage: alpha-test reject preserves fragment state

Status: **Tightened alpha-test rejection coverage**

Direct reference-emulator implementation use for this check: **0**. This is a
coverage pass around the existing alpha-test rule: a fragment whose effective
alpha is less than or equal to `ALPHA_TEST_REF` is discarded before it can write
color or side buffers.

### Why this mattered

`test_alpha_test_requires_alpha_greater_than_ref` already proved rejected
fragments do not become visible. It did not prove that rejection leaves the
existing pixel's state untouched. Commercial games commonly use alpha-tested
texture cutouts; if a rejected texel changed depth, polygon ID, fog, alpha, or
translucent bookkeeping, later edge marking, fog, AA, or overlap behavior could
diverge even though the rejected texel itself was invisible.

### Coverage added

Added `test_alpha_test_reject_preserves_existing_fragment_state`. The fixture:

- draws an opaque, fog-enabled base polygon;
- submits a nearer translucent polygon with alpha equal to `ALPHA_TEST_REF`;
- verifies the rejected fragment leaves color, alpha, depth, polygon ID, fog
  enable state, and translucent-ID state unchanged.

Verification:

```sh
cargo test -p nds-core test_alpha_test_reject_preserves_existing_fragment_state --release
cargo test -p nds-core alpha_test --release
```

Result:

- Focused alpha-test reject state test: `1 passed; 0 failed`.
- Alpha-test focused release tests: `4 passed; 0 failed`.

## 2026-06-06 validation: latest HeartGold title smoke after renderer coverage

Status: **Re-ran current HeartGold frame capture after the latest raster-state tests**

This is an image-level smoke check, not a new implementation change and not a
claim of full visual conformance. It verifies that the recent depth,
translucency, alpha-test, fog, edge-marking, and capture coverage additions did
not regress the commercial title scene that motivated this 3D pass.

Command:

```sh
cargo run --release -p nds-frontend -- --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 4320 --capture-ppm /private/tmp/heartgold-20260606-latest.ppm
```

Result:

- Capture completed successfully and wrote
  `/private/tmp/heartgold-20260606-latest.ppm`.
- Converted inspection copy:
  `/private/tmp/heartgold-20260606-latest.png`.
- The capture is `256 x 384`, matching the compact two-screen layout with no
  artificial gap.
- Visual inspection shows a coherent title frame: Ho-Oh and the "TOUCH TO
  START" prompt on the top screen, the HeartGold logo and Game Freak text on
  the bottom screen, and no recurrence of the earlier random polygon flashing.

## 2026-06-06 validation: HeartGold title-loop sequence sweep

Status: **Extended the current validation from one still frame to a short title-loop sweep**

This was a runtime/image-level check only. No reference-emulator code was used
for this pass, and this is still not a claim of full 3D conformance.

Command:

```sh
cargo run --release -p nds-frontend -- --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 5400 --capture-interval 540 --capture-dir /private/tmp/heartgold-20260606-sweep
```

Result:

- Capture completed through frame 5400.
- Wrote ten native `256 x 384` PPM frames:
  `/private/tmp/heartgold-20260606-sweep/frame-000540.ppm` through
  `/private/tmp/heartgold-20260606-sweep/frame-005400.ppm`.
- Converted and inspected representative samples:
  - frame 540: expected Game Freak splash.
  - frame 2700: coherent transition/title artwork.
  - frame 4320: clean Ho-Oh title frame with "TOUCH TO START".
  - frame 5400: clean later Ho-Oh title frame.
- The sequence preserves the compact top/bottom screen layout with no large
  artificial gap.
- The sampled frames did not reproduce the earlier random-polygon flashing.

Remaining limitation:

- This confirms that the current build no longer reproduces the visible failure
  in this sampled HeartGold title sequence. It does not prove pixel-level
  accuracy against DS hardware or a trusted reference capture, and intermittent
  visual bugs outside these sampled frames remain possible.

## 2026-06-06 coverage: 256x128 display-capture height cutoff

Status: **Tightened display-capture size coverage**

Direct reference-emulator implementation use for this check: **0**. This is a
straightforward invariant from the `DISPCAPCNT` size field: 256x128 capture uses
the normal 256-pixel row stride, but it must stop writing after line 127.

### Why this mattered

The 128x128 compact-stride and 256x64 short-height cases were already covered.
The remaining short 256-wide size, 256x128, should follow the same stride rule
as 256x64 and the same height-cutoff behavior at a different boundary. Without
a focused test, a future cleanup could accidentally treat all short captures as
128-wide, or write line 128 into the next capture row.

### Coverage added

Added `test_display_capture_256x128_uses_screen_stride_and_stops_at_height`.
The fixture:

- configures source-A capture to VRAM B with capture size 256x128;
- writes distinct source pixels at lines 127 and 128;
- verifies line 127 lands at the 256-wide row offset;
- verifies line 128 is not written;
- verifies the capture busy/active state remains set until the visible frame
  ends, matching the existing short-capture behavior.

Verification:

```sh
cargo test -p nds-core display_capture --release
```

Result:

- Display-capture focused release tests: `10 passed; 0 failed`.

## 2026-06-06 coverage: alpha-test reject preserves line and zero-dot state

Status: **Tightened alpha-test rejection coverage for special primitives**

Direct reference-emulator implementation use for this check: **0**. This is a
coverage pass around an existing hardware-facing rule: with alpha test enabled,
fragments whose effective alpha is less than or equal to `ALPHA_TEST_REF` must
be discarded before they update color, depth, polygon ID, fog state, edge/AA
bookkeeping, or translucent-overlap state.

### Why this mattered

Normal filled triangles already had state-preservation coverage for alpha-test
rejection. The rasterizer has separate draw paths for degenerate line segments
and zero-dot polygons, both of which are observable in DS 3D because the
hardware supports one-dot/wire/degenerate output cases. If those paths skipped
the visible color write but still changed side buffers, later fog, edge marking,
AA, or translucent same-ID rejection could diverge in commercial scenes that use
alpha-tested cutouts or tiny geometry.

### Coverage added

Added:

- `test_alpha_test_reject_line_preserves_existing_fragment_state`
- `test_alpha_test_reject_zero_dot_preserves_existing_fragment_state`

Both fixtures:

- draw an opaque, fog-enabled base polygon;
- submit a nearer alpha-tested special primitive with alpha equal to
  `ALPHA_TEST_REF`;
- verify the rejected primitive leaves framebuffer color, alpha, depth, polygon
  ID, fog enable state, and translucent-ID state unchanged.

Verification:

```sh
cargo test -p nds-core alpha_test --release
```

Result:

- Alpha-test focused release tests: `6 passed; 0 failed`.

## 2026-06-06 coverage: same-ID translucent reject preserves line and zero-dot state

Status: **Tightened translucent same-ID rejection coverage for special primitives**

Direct reference-emulator implementation use for this check: **0**. This is a
coverage pass around the DS translucent-overlap rule already implemented in the
rasterizer: a translucent fragment with the same polygon ID as a previous
translucent fragment at the same pixel must be rejected so it does not blend
twice.

### Why this mattered

The filled-triangle path already proved rejected same-ID translucent fragments
leave the existing fragment state untouched. Degenerate line segments and
zero-dot polygons go through separate draw paths. If those paths rejected color
but still changed alpha, fog, or translucent-ID bookkeeping, later edge marking,
fog, AA, or additional translucent overlap could diverge from hardware in small
geometry and wire/one-dot cases.

### Coverage added

Added:

- `test_same_id_translucent_line_reject_preserves_fragment_state`
- `test_same_id_translucent_zero_dot_reject_preserves_fragment_state`

Both fixtures:

- draw an opaque, fog-enabled base polygon;
- draw one translucent special primitive with polygon ID 7;
- draw a second translucent special primitive with the same polygon ID but a
  different color and fog-disabled state;
- verify the second primitive is rejected and leaves framebuffer color, alpha,
  fog enable state, and translucent-ID state from the first translucent fragment.

Verification:

```sh
cargo test -p nds-core translucent --release
```

Result:

- Translucent focused release tests: `35 passed; 0 failed`.

## 2026-06-06 coverage: translucent depth update for line and zero-dot primitives

Status: **Tightened translucent depth-update coverage for special primitives**

Direct reference-emulator implementation use for this check: **0**. This is a
coverage pass around `POLYGON_ATTR` bit 11, which allows translucent fragments
to update the depth buffer. The normal filled-triangle path already had coverage
showing that a depth-updating translucent fragment can occlude a later
translucent fragment behind it.

### Why this mattered

Degenerate lines and zero-dot polygons go through separate draw paths from
filled triangles. If those paths blended color but failed to update depth when
bit 11 is set, a later translucent polygon behind them could blend through small
wire/one-dot geometry even though hardware should reject it by depth. This kind
of error is visually subtle but can show up as flickering or halos around tiny
3D details.

### Coverage added

Added:

- `test_translucent_line_depth_update_occludes_later_translucent_fragment`
- `test_translucent_zero_dot_depth_update_occludes_later_translucent_fragment`

Both fixtures:

- draw an opaque base polygon;
- draw a nearer translucent line/zero-dot primitive with depth-update enabled;
- draw a later translucent polygon behind that primitive;
- verify the later polygon does not blend through the depth-updating special
  primitive;
- verify the depth buffer stores the special primitive's front depth.

Verification:

```sh
cargo test -p nds-core translucent --release
```

Result:

- Translucent focused release tests: `37 passed; 0 failed`.

## 2026-06-06 coverage: zero-dot AA state is cleared by later real geometry

Status: **Tightened anti-alias zero-dot side-buffer coverage**

Direct reference-emulator implementation use for this check: **0**. This is a
side-state regression around the internal `zero_dot_buffer`, which marks opaque
one-dot polygons so the AA pass can hide them unless edge marking is also
enabled.

### Why this mattered

Zero-dot polygons are special: with AA enabled and edge marking disabled, the
post-pass hides opaque one-dot pixels. That decision is based on side-buffer
state captured during rasterization. If a later line or filled triangle
overwrites the same pixel but fails to clear the stale zero-dot marker, AA can
incorrectly hide real geometry. This would show up as missing pixels or
flickering gaps in tiny wire/edge details.

### Coverage added

Added:

- `test_line_over_zero_dot_clears_zero_dot_antialias_state`
- `test_triangle_over_zero_dot_clears_zero_dot_antialias_state`

Both fixtures:

- draw an opaque zero-dot polygon behind the target pixel;
- draw nearer real geometry over that same pixel;
- run with AA enabled;
- verify the later line/triangle remains visible;
- verify `zero_dot_buffer` is cleared at the overwritten pixel.

Verification:

```sh
cargo test -p nds-core antialias --release
```

Result:

- Anti-alias focused release tests: `19 passed; 0 failed`.

## 2026-06-06 fix: line and zero-dot writes clear stale AA coverage hints

Status: **Fixed stale anti-alias coverage metadata when special primitives overwrite triangle pixels**

Direct reference-emulator implementation use for this fix: **0**. The issue was
found by auditing the local draw paths: filled triangles update
`aa_coverage_buffer` / `aa_edge_hint_buffer`, but line and zero-dot writes
previously did not clear those buffers when they overwrote the same pixel.

### Symptom

A line or edge-marked zero-dot primitive drawn over a partially covered triangle
pixel could inherit the triangle's old AA coverage value. In the focused
regression, a line overwrote a triangle pixel with stale coverage `8`; the AA
post-pass then used alpha `8` instead of the fallback line coverage `16`.

This is visible-risky because stale coverage can make later real geometry too
transparent or blend toward the wrong edge, creating small gaps or shimmering
around wire/one-dot details.

### Root cause

The filled-triangle scanline path calls `update_aa_coverage(...)`, which either
records fresh coverage/hints for opaque fractional pixels or clears the buffers.
The line and zero-dot paths updated color, alpha, depth, ID, edge, fog, and
zero-dot state, but left previous AA coverage/hint bytes untouched.

### Fix

Added `clear_aa_coverage(...)` and call it from:

- the line write path;
- the zero-dot write path.

Filled triangle pixels still use `update_aa_coverage(...)` because they can
carry real fractional coverage. Special primitives now fall back to the AA
post-pass default instead of inheriting stale triangle metadata.

### Regression coverage

Added:

- `test_line_over_triangle_clears_stale_antialias_coverage`
- `test_zero_dot_over_triangle_clears_stale_antialias_coverage`

Verification:

```sh
cargo test -p nds-core test_line_over_triangle_clears_stale_antialias_coverage --release
cargo test -p nds-core antialias --release
```

Result:

- Focused stale-coverage regression: first failed with stale alpha `8`, then
  passed after the fix.
- Anti-alias focused release tests: `21 passed; 0 failed`.

## 2026-06-06 fix: invalid packed GX command bytes do not terminate the packed word

Status: **Fixed GXFIFO packed-command decoder conformance**

Direct reference-emulator implementation use for this fix: **0**. This came
from checking the local packed-command decoder against GBATEK's GXFIFO command
byte rule: command byte `00h` is padding/terminator, but invalid nonzero
command bytes are simply ignored and do not fetch parameters.

### Symptom / gap

The packed decoder previously treated an invalid command byte like `00h`. That
terminated the rest of the packed command word, so later valid command bytes in
the same word were dropped.

Example:

```text
packed word bytes, LSB first:
11 FF 15 12

old local decode:
MTX_PUSH

hardware-facing decode:
MTX_PUSH
ignore FF
MTX_IDENTITY
MTX_POP param follows
```

### Fix

- Kept `00h` as the packed-word terminator/padding byte.
- Changed invalid nonzero command bytes to `continue` instead of `break`.
- Invalid command bytes do not add FIFO entries and do not consume parameter
  words.

Why this matters:

Games and command-list generators normally emit valid bytes, but FIFO
conformance matters for DMA-fed command streams, malformed padding, and test
ROMs that probe command decoder behavior. Dropping valid later command bytes can
change matrix/vertex state for the rest of the frame.

Regression coverage:

```text
test_packed_word_invalid_command_byte_is_ignored_without_terminating
```

Verification:

```sh
cargo test -p nds-core packed_word_invalid --release
```

Result:

- Focused invalid packed-command release test: `1 passed; 0 failed`.

## 2026-06-06 fix: GXSTAT high readback reports GXFIFO full

Status: **Fixed GXFIFO status readback conformance**

Direct reference-emulator implementation use for this fix: **0**. This came
from auditing the two local FIFO status helpers against the documented GXSTAT
high bits. `stat_high()` already exposed the full bit, but the engine's actual
`gxstat_high()` path uses `gxstat_high_bits(...)`, which omitted it.

### Symptom / gap

When the emulated FIFO had at least `256` entries, the visible count saturated
to `256`, but `GXSTAT[24]` was not explicitly set through the engine readback
path. Software polling the full bit could therefore see an impossible status:
a saturated FIFO count without the full flag.

### Fix

- `GxFifo::gxstat_high_bits(...)` now sets bit 8 of the high halfword when
  `entries >= FIFO_CAPACITY`.
- The existing over-capacity preservation behavior is unchanged: the emulator
  keeps command words instead of modeling ARM9 write stalls, while still
  capping the hardware-visible count.

Why this matters:

Games poll `GXSTAT` to pace command submission and DMA command-list feeding.
Accurate full/half/empty flags keep those loops aligned with DS hardware even
when the emulator preserves over-capacity writes internally.

Regression coverage:

```text
test_direct_port_write_past_full_preserves_command_stream
```

Verification:

```sh
cargo test -p nds-core direct_port_write_past_full --release
```

Result:

- Focused full-FIFO status release test: `1 passed; 0 failed`.

## 2026-06-06 coverage: W-buffer ordering for line and zero-dot primitives

Status: **Tightened W-buffer depth coverage for special primitives**

Direct reference-emulator implementation use for this check: **0**. This is a
coverage pass around the existing W-buffer depth path. Filled triangles already
had coverage proving that, in W-buffer mode, depth ordering must use clip W
rather than Z.

### Why this mattered

Degenerate line segments and zero-dot polygons use separate draw paths from
filled triangles. If those paths accidentally used Z depth while filled
triangles used W depth, small wire/one-dot details could draw in front of or
behind the wrong polygons in scenes that enable W-buffering. That kind of error
would be visible as tiny depth pops or flickering outlines.

### Coverage added

Added:

- `test_w_buffering_orders_degenerate_line_by_w`
- `test_w_buffering_orders_zero_dot_by_w`

Both fixtures:

- enable W-buffering;
- draw a Z-near/W-far special primitive first;
- draw a Z-far/W-near special primitive second at the same pixel;
- verify the W-near primitive wins, proving ordering follows W rather than Z.

Verification:

```sh
cargo test -p nds-core w_buffering --release
```

Result:

- W-buffer focused release tests: `5 passed; 0 failed`.

## 2026-06-06 coverage: W-buffer equal-depth tolerance for line and zero-dot primitives

Status: **Tightened W-buffer equal-depth coverage for special primitives**

Direct reference-emulator implementation use for this check: **0**. This is a
coverage pass around `POLYGON_ATTR` bit 14, the equal-depth mode that accepts
incoming fragments within the hardware depth tolerance window.

### Why this mattered

Filled triangles already had W-buffer equal-depth coverage for the inclusive
tolerance boundary and the rejection just outside it. Degenerate line segments
and zero-dot polygons use separate draw paths. If those paths ignored the
equal-depth tolerance, small wire/one-dot details could disappear when drawn at
nearly the same W depth as the surface they are meant to decorate.

### Coverage added

Added:

- `test_w_buffering_depth_equal_allows_later_line_within_tolerance`
- `test_w_buffering_depth_equal_allows_later_zero_dot_within_tolerance`

Both fixtures:

- enable W-buffering;
- draw a base special primitive at W=4096;
- draw a later same-pixel special primitive at W=4608 with equal-depth mode
  enabled;
- verify the later primitive wins, proving the inclusive tolerance path applies
  to line and zero-dot rasterization too.

Verification:

```sh
cargo test -p nds-core w_buffering --release
```

Result:

- W-buffer focused release tests: `7 passed; 0 failed`.

## 2026-06-06 coverage: opaque line and zero-dot writes clear stale fog state

Status: **Tightened fog side-buffer coverage for special primitives**

Direct reference-emulator implementation use for this check: **0**. This is a
coverage pass around `fog_enable_buffer`, which controls whether the fog
post-pass modifies a rendered pixel.

### Why this mattered

Translucent line and zero-dot paths already had coverage proving they AND their
fog flag with the existing framebuffer fog flag. The opaque overwrite case was
not explicitly covered. If an opaque line or zero-dot polygon without fog
overwrote a fog-enabled base pixel but failed to clear the stale fog flag, the
post-pass would incorrectly fog the new geometry. That can produce darkened
wire/one-dot details or small alpha/color artifacts.

### Coverage added

Added:

- `test_opaque_line_clears_stale_fog_flag`
- `test_opaque_zero_dot_clears_stale_fog_flag`

Both fixtures:

- enable fog with full-density black fog;
- draw a fog-enabled base polygon;
- draw a nearer opaque special primitive with fog disabled;
- verify the final special-primitive color remains unfogged;
- verify `fog_enable_buffer` is cleared at the overwritten pixel.

Verification:

```sh
cargo test -p nds-core fog --release
```

Result:

- Fog focused release tests: `21 passed; 0 failed`.

## 2026-06-06 coverage: alpha-zero texel skips preserve side buffers

Status: **Tightened transparent-texture skip coverage**

Direct reference-emulator implementation use for this check: **0**. This is a
local invariants pass around the rasterizer's own side buffers, not a port from
a reference implementation.

### Why this mattered

The rasterizer already skipped fragments whose texture/color combine resolved
to effective alpha `0`. The previous wireframe A5I3 alpha-zero regression only
checked that the visible framebuffer bit stayed clear. That left a blind spot:
a skipped fragment must also leave the hidden metadata untouched. If the skip
path mutated depth, polygon ID, translucent ID, edge/fog flags, AA coverage
hints, or zero-dot state, later post-effects could still see a "ghost" fragment
even though no visible color was written.

That class of bug is especially relevant to the HeartGold title investigation
because the failures have shown up as intermittent edge/fog/AA-style artifacts,
not as simple solid-color geometry errors.

### Coverage added

Added a small `PixelState` snapshot helper and three direct rasterizer tests:

- `test_filled_a5i3_alpha_zero_texel_preserves_existing_fragment_state`
- `test_wireframe_a5i3_alpha_zero_texel_preserves_existing_fragment_state`
- `test_zero_dot_a5i3_alpha_zero_texel_preserves_existing_fragment_state`

Each fixture:

- enables texture mapping;
- seeds all per-pixel color/depth/ID/post-effect side buffers at the target
  pixel;
- rasterizes a nearer A5I3 alpha-zero fragment through one raster path;
- verifies the full seeded state is unchanged.

Verification:

```sh
cargo test -p nds-core alpha_zero --release
```

Result:

- Alpha-zero focused release tests: `6 passed; 0 failed`.

## 2026-06-06 coverage: T-axis repeat+flip raster texture sampling

Status: **Tightened vertical texture-addressing coverage**

Direct reference-emulator implementation use for this check: **0**. This is a
local coverage pass derived from the documented `TEXIMAGE_PARAM` repeat/flip
bits and the existing S-axis raster test.

### Why this mattered

The texture sampler already had unit coverage for repeat+flip coordinate
wrapping and the raster path already had an S-axis repeat+flip marker test.
That still left a raster-path asymmetry: a regression could apply repeat+flip
correctly to S while mishandling T after perspective-correct coordinate
recovery. Vertical texture-addressing bugs would show up as distorted title
art/backgrounds even if horizontal marker tests stayed green.

### Coverage added

Added:

- `test_texture_t_repeat_flip_bits_are_applied_during_raster_sampling`

The fixture:

- creates an 8x8 direct-color marker texture;
- places different colors at row 1, row 6, and row 7;
- rasterizes a scanline whose recovered T coordinate is `9`;
- enables T repeat+flip only;
- verifies the sample mirrors to row `6`, not row `1` from plain repeat or row
  `7` from clamp.

Verification:

```sh
cargo test -p nds-core repeat_flip --release
```

Result:

- Repeat/flip focused release tests: `3 passed; 0 failed`.

## 2026-06-06 validation: current HeartGold 5400-frame title sweep

Status: **Revalidated visible title-loop stability from the current release binary**

Direct reference-emulator implementation use for this check: **0**. This was a
local runtime capture from the current `./target/release/nds-frontend` binary.

### Why this mattered

The remaining conformance gap is not only unit-test coverage. We also need
periodic image-level validation against the real commercial title that exposed
the 3D failures. Earlier symptoms included a black screen, a very tall frontend
layout gap, and intermittent random polygon flashing around the title scene.

### Validation run

Command:

```sh
./target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 5400 --capture-interval 540 --capture-dir /private/tmp/heartgold-20260606-current-verify
```

Output:

- ten PPM frames from `frame-000540.ppm` through `frame-005400.ppm`;
- each frame is `256 x 384`;
- representative frames were converted to PNG with `sips` for inspection.

Observed frames:

- `frame-000540`: expected Game Freak splash.
- `frame-002700`: coherent title-animation character art.
- `frame-004320`: Ho-Oh title scene with bottom HeartGold logo and `TOUCH TO
  START`.
- `frame-005400`: later Ho-Oh title scene with bottom HeartGold logo and
  `TOUCH TO START`.

Result:

- No black-screen regression in the sampled sequence.
- No oversized frontend screen gap; output is the compact two-screen stack.
- No random polygon flashing in the inspected sampled frames.
- This is supporting image evidence only. It does not prove full 3D visual
  conformance without reference/hardware image comparison.

## 2026-06-06 tool: PPM capture comparator

Status: **Added repeatable image-diff utility**

Direct reference-emulator implementation use for this tool: **0**. This is a
local validation utility for comparing emulator captures against separately
produced reference images.

### Why this mattered

The project already had deterministic PPM capture, and the latest HeartGold
sequence was inspected manually. Manual inspection is useful for gross failures
like black screens or random polygon flashing, but it cannot prove visual
conformance. The next step is a mechanical comparison path that reports how
far a current capture is from a reference capture and produces a diff image
for inspection.

### Tool added

Added:

```text
tools/compare_ppm.py
```

The tool:

- reads binary `P6` PPM captures without third-party dependencies;
- verifies matching dimensions;
- reports changed pixels, changed channels, max channel delta, and RMSE;
- compares either one PPM file pair or two `--capture-dir` directories by
  matching `frame-000000.ppm` style filenames;
- supports `--pixel-threshold`, `--max-changed-pixels`, and
  `--max-channel-delta` failure gates;
- can write an amplified diff PPM with `--write-diff`, or a directory of diff
  PPMs when comparing capture directories.

The test file uses only Python's standard `unittest` module and covers:

- identical file comparisons;
- different file comparisons plus diff output;
- directory sequence comparisons that ignore non-sequence diff artifacts;
- missing-frame errors.

The manifest runner accepts JSON cases so reference comparisons can be encoded
as named checks instead of hand-written command lines:

```json
{
  "cases": [
    {
      "name": "heartgold-title",
      "actual": "/tmp/current-heartgold-seq",
      "reference": "/tmp/reference-heartgold-seq",
      "pixel_threshold": 0,
      "max_changed_pixels": 0,
      "max_channel_delta": 0,
      "write_diff": "/tmp/heartgold-title-diff",
      "ignore_metadata": false
    }
  ]
}
```

Run with:

```sh
python3 tools/run_visual_manifest.py /tmp/visual-manifest.json
```

`tools/visual_manifest.example.json` contains a checked-in template for both
sequence and single-frame visual comparisons.

By default the runner validates capture sidecars before comparing pixels:

- single captures use `<capture>.json`;
- sequence captures use `capture-metadata.json`;
- metadata format, kind, ROM identity, frame window, interval, screen gap,
  output dimensions, and listed sequence files must match.

Legacy captures made before metadata sidecars can still be compared by setting
`"ignore_metadata": true` in the manifest case.

Added Python cache ignores to `.gitignore`:

```text
__pycache__/
*.py[cod]
```

### Verification

Identical-frame check:

```sh
python3 tools/compare_ppm.py /private/tmp/heartgold-20260606-current-verify/frame-004320.ppm /private/tmp/heartgold-20260606-current-verify/frame-004320.ppm
```

Result:

- `changed_pixels: 0`
- `changed_channels: 0`
- `max_channel_delta: 0`
- `rmse: 0.0000`
- exit code `0`

Different-frame check:

```sh
python3 tools/compare_ppm.py /private/tmp/heartgold-20260606-current-verify/frame-005400.ppm /private/tmp/heartgold-20260606-current-verify/frame-004320.ppm --write-diff /private/tmp/heartgold-20260606-current-verify/frame-005400-vs-004320-diff.ppm
```

Result:

- `changed_pixels: 80888 (82.2835%)`
- `changed_channels: 212304`
- `max_channel_delta: 255`
- `rmse: 71.8376`
- exit code `1`
- generated diff file is a valid `256 x 384` PPM.

Directory self-check:

```sh
python3 tools/compare_ppm.py /private/tmp/heartgold-20260606-current-verify /private/tmp/heartgold-20260606-current-verify
```

Result:

- matched ten capture files from `frame-000540.ppm` through
  `frame-005400.ppm`;
- all per-frame deltas were zero;
- summary `changed_pixels: 0`, `max_channel_delta: 0`, `rmse: 0.0000`;
- exit code `0`.

Directory mismatch check:

```sh
python3 tools/compare_ppm.py /private/tmp/heartgold-20260606-current-verify /private/tmp/heartgold-20260606-shifted-verify --write-diff /private/tmp/heartgold-20260606-seq-diff
```

Result:

- nine frames matched exactly;
- `frame-005400.ppm` reported `80888` changed pixels, max channel delta `255`,
  and RMSE `71.8376`;
- summary RMSE was `22.7170`;
- exit code `1`;
- generated per-frame diff PPMs, including a valid `256 x 384`
  `frame-005400.ppm` diff.

Unit test check:

```sh
python3 -m unittest tools/compare_ppm_test.py
python3 -m unittest tools/run_visual_manifest_test.py
```

Result:

- Comparator tests: `4` passed.
- Manifest-runner tests: `6` passed.

Independent capture determinism check:

```sh
./target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 2160 --capture-interval 540 --capture-dir /private/tmp/heartgold-20260606-determinism-a
./target/release/nds-frontend --rom /Users/lijunzhang/Documents/Pokemon-HeartGoldVersionUSA.nds --no-audio --capture-frames 2160 --capture-interval 540 --capture-dir /private/tmp/heartgold-20260606-determinism-b
python3 tools/compare_ppm.py /private/tmp/heartgold-20260606-determinism-a /private/tmp/heartgold-20260606-determinism-b
```

Result:

- both captures produced `frame-000540.ppm`, `frame-001080.ppm`,
  `frame-001620.ppm`, and `frame-002160.ppm`;
- all four independent capture pairs matched exactly;
- summary `changed_pixels: 0`, `changed_channels: 0`,
  `max_channel_delta: 0`, `rmse: 0.0000`;
- exit code `0`.

This means the current direct-boot capture path is deterministic for this
HeartGold ROM/save/frame window. With controlled inputs, future reference
captures can use strict zero-delta gates before relaxing thresholds for
known-timing or reference-source differences.

Manifest runner check:

```sh
python3 tools/run_visual_manifest.py /private/tmp/heartgold-20260606-determinism-manifest.json
```

Result:

- `PASS heartgold-determinism`;
- `frames=4`;
- `changed_pixels=0`;
- `changed_channels=0`;
- `max_channel_delta=0`;
- `worst_rmse=0.0000`;
- generated four valid `256 x 384` per-frame diff PPMs in
  `/private/tmp/heartgold-20260606-determinism-manifest-diff`.

Metadata-aware manifest checks:

```sh
python3 tools/run_visual_manifest.py /private/tmp/heartgold-20260606-metadata-smoke-manifest.json
python3 tools/run_visual_manifest.py /private/tmp/heartgold-20260606-determinism-legacy-manifest.json
python3 tools/run_visual_manifest.py /private/tmp/heartgold-20260606-identity-metadata-manifest.json
```

Result:

- metadata-enabled smoke capture self-comparison passed with `frames=2` and
  zero deltas after sidecar validation;
- older determinism captures passed with explicit `"ignore_metadata": true`,
  `frames=4`, and zero deltas.
- ROM identity metadata self-comparison passed with `frames=2`, zero deltas,
  `rom_size=134217728`, `rom_title=POKEMON HG`, `gamecode=IPKE`, and
  `header_crc_valid=true`.

Full tool and workspace verification after the metadata identity fields:

```sh
python3 -m unittest tools/compare_ppm_test.py tools/run_visual_manifest_test.py
cargo test --workspace --release
```

Result:

- Python visual tooling tests: `10` passed.
- Rust workspace release tests: `nds-core` `627` passed,
  `nds-frontend` `7` passed, doctests `0` passed.

This does not finish visual conformance. It gives the repo the missing
mechanical gate needed once trusted reference captures are available.

## 2026-06-06 indoor OBJ shadow corruption

Latest Desktop screenshots inspected:

```text
Screenshot 2026-06-06 at 5.01.08 PM.png
Screenshot 2026-06-06 at 5.01.19 PM.png
Screenshot 2026-06-06 at 5.02.23 PM.png
```

Observed behavior:

- Outdoor player/map screenshot looked normal.
- Indoor screenshots showed a black striped block or solid black block around
  the player/stair area.
- The indoor room background rendered mostly correctly, which made this look
  like an OBJ-layer problem rather than a BG tilemap or 3D geometry problem.

Root cause:

- NDS OBJ `gfx_mode = 3` is bitmap OBJ mode.
- The renderer documented that mode, but still decoded those sprites through
  the indexed/tiled 4bpp/8bpp path.
- That treats direct-color bitmap data as palette indices, so transparent
  bitmap pixels can become black OBJ pixels and bitmap shadow/fade sprites can
  become solid black blocks.
- Bitmap OBJ attr2 bits `12-15` are an OAM alpha value, not an indexed OBJ
  palette bank.

Fix:

- `gpu2d::obj` now decodes bitmap OBJs as direct-color pixels from OBJ VRAM.
- Bitmap pixels with bit 15 clear are transparent.
- Visible bitmap pixels output `color & 0x7FFF`.
- Bitmap OBJ alpha from attr2 bits `12-15` is carried into the compositor.
- Bitmap OBJ addressing now follows the separate bitmap mapping controls from
  `DISPCNT` bits `6`, `5`, and `22` instead of reusing tile OBJ mapping bit
  `4`.
- `gpu2d::compositor` blends bitmap OBJ pixels with their second target using
  the bitmap OAM alpha coefficient.

Focused coverage:

- `test_bitmap_obj_reads_direct_color_and_alpha` verifies direct-color bitmap
  OBJ reads, bit15 transparency, and attr2 alpha extraction.
- `test_bitmap_obj_2d_256_mapping_uses_dispcnt5_source_width` verifies
  bitmap 2D/256-dot source-width addressing.
- `test_bitmap_obj_1d_256_mapping_uses_dispcnt22_boundary` verifies bitmap
  1D/256-byte boundary addressing.
- `test_bitmap_obj_uses_oam_alpha_over_second_target` verifies compositor
  blending through the bitmap OBJ alpha path.

Targeted verification:

```sh
cargo test -p nds-core gpu2d --release
```

Result:

- Initial direct-color/alpha coverage: `13` GPU2D-focused tests passed.
- After correcting bitmap mapping bits: `15` GPU2D-focused tests passed.

### Follow-up: DS OBJ priority ordering

While reviewing GBATEK's DS OBJ rules for bitmap OBJs, another visible sprite
ordering issue turned up. DS mode combines the 2-bit OBJ priority field with
the OAM entry number. Our renderer was using the first nontransparent OBJ pixel
encountered, so a later sprite with a higher visual priority could be hidden
behind an earlier lower-priority sprite.

Fix:

- `ObjPixel` now records the source OAM index.
- OBJ pixels now replace an existing pixel when they have a lower numeric OBJ
  priority.
- Equal priority keeps the lower OAM index, matching DS tie behavior.

Focused coverage:

- `test_later_obj_with_higher_priority_replaces_earlier_obj_pixel` verifies
  that later high-priority sprites can appear above earlier low-priority ones.
- `test_equal_obj_priority_keeps_lower_oam_index` verifies the OAM-index
  tie-breaker.

Targeted verification:

```sh
cargo test -p nds-core gpu2d --release
```

Result:

- After OBJ priority ordering coverage: `17` GPU2D-focused tests passed.

### Follow-up: DS OBJ vertical wrap

GBATEK also calls out a DS-specific OBJ vertical wrap difference from GBA mode:
large OBJs near the bottom of the OBJ coordinate space can appear in both the
bottom and top visible portions when their box crosses the 256-line boundary.
The renderer previously converted Y values `>= 192` to negative coordinates and
then used a simple visible-line subtraction. That handled some negative-style
placements but did not model the DS 256-line OBJ coordinate wrap directly.

Fix:

- OBJ source row selection now computes `(visible_line - obj_y) mod 256`.
- A sprite draws on the visible line when that wrapped row is inside the
  normal or affine/double-size OBJ box height.
- This preserves ordinary in-bounds OBJs while allowing DS-style top wrapping
  for boxes crossing the 256-line boundary.

Focused coverage:

- `test_obj_row_in_box_wraps_across_256_line_boundary` verifies bottom and top
  row selection for a large wrapped OBJ box.
- `test_obj_y_wrap_draws_top_screen_portion` verifies the renderer samples the
  wrapped top-screen source row for an OBJ starting at Y=252.

Targeted verification:

```sh
cargo test -p nds-core gpu2d --release
```

Result:

- After OBJ vertical-wrap coverage: `19` GPU2D-focused tests passed.

### Follow-up: OBJ mosaic

The OBJ renderer parsed attr0 bit 12 but ignored it. DS/GBA OBJ mosaic uses
the OBJ horizontal and vertical sizes in the `MOSAIC` register to reuse the
source pixel at the top-left of each mosaic cell. Ignoring this can make sprite
effects too sharp or misaligned when games use OBJ mosaic for transitions,
shadows, or visual effects.

Fix:

- Regular OBJs now snap source `x` and `y` coordinates to the current OBJ
  mosaic cell when attr0 mosaic is enabled.
- Affine OBJs now snap the box-space sample coordinate before applying the
  affine matrix.
- Non-mosaic OBJs keep the previous sampling path.

Focused coverage:

- `test_obj_mosaic_reuses_left_cell_pixel` verifies horizontal OBJ mosaic.
- `test_obj_mosaic_reuses_top_cell_row` verifies vertical OBJ mosaic.

Targeted verification:

```sh
cargo test -p nds-core gpu2d --release
```

Result:

- After OBJ mosaic coverage: `21` GPU2D-focused tests passed.

### Follow-up: forced first-target effects for semi-transparent and bitmap OBJs

GBATEK notes that semi-transparent OBJs are always selected as a first target,
regardless of `BLDCNT` bit 4, and always use alpha blending when they overlap a
valid second target. The previous compositor handled the overlap case, but when
there was no second target it fell back to the normal first-target check. That
meant brightness-up/down effects could be skipped for semi-transparent OBJ
pixels unless the game also set `BLDCNT`'s OBJ first-target bit.

Bitmap OBJ mode uses direct-color pixels plus an OAM alpha coefficient in attr2
bits 12-15. The compositor already used that coefficient for valid second
targets. The same top-OBJ forced-first-target rule now also lets bitmap OBJ
pixels receive brightness effects when they do not overlap a second target.

Fix:

- Semi-transparent OBJ pixels still alpha-blend over a selected second target
  before brightness is considered.
- Bitmap OBJ pixels still use their OAM alpha coefficient over a selected
  second target.
- If no valid second target is present, semi-transparent and bitmap OBJ pixels
  are treated as forced first targets for `BLDCNT` brightness effects.
- Normal OBJ/BG first-target behavior is unchanged.

Focused coverage:

- `test_semitransparent_obj_brightness_is_forced_first_target_without_bldcnt_obj_bit`
  verifies semi-transparent OBJ brightness even when `BLDCNT` bit 4 is clear.
- `test_bitmap_obj_brightness_is_forced_first_target_without_second_target`
  verifies the same no-second-target brightness path for bitmap OBJs.
- `test_bitmap_obj_uses_oam_alpha_over_second_target` verifies the existing
  bitmap-alpha blend path still takes priority when a second target exists.

Targeted verification:

```sh
cargo test -p nds-core obj_brightness --release
cargo test -p nds-core bitmap_obj_uses_oam_alpha --release
cargo test -p nds-core gpu2d --release
cargo test -p nds-core gpu3d --release
```

Result:

- OBJ brightness focused tests: `2` passed.
- Bitmap OBJ alpha focused test: `1` passed.
- GPU2D focused tests: `23` passed.
- GPU3D focused tests: `322` passed.

### Follow-up: window color-effects gate blocks forced OBJ blending

GBATEK's window feature lets each window region independently enable BG/OBJ
layers and color special effects. Semi-transparent OBJ and bitmap OBJ alpha are
special-effect paths, so they must not bypass a window region whose effects bit
is clear.

The previous compositor still allowed semi-transparent OBJ alpha blending and
bitmap OBJ alpha blending in the `effects_enable = false` branch. That could
make windowed UI or room overlays blend when the game intended those pixels to
draw as normal top OBJ color.

Fix:

- When the active window region disables color effects, the compositor now
  returns the top pixel color directly.
- Forced first-target handling for semi-transparent and bitmap OBJs remains
  active only when the active window region enables effects.

Focused coverage:

- `test_window_effects_disable_blocks_semitransparent_obj_blend` verifies that
  a semi-transparent OBJ over a selected second target does not blend inside a
  window with effects disabled.
- `test_window_effects_disable_blocks_bitmap_obj_alpha_blend` verifies the
  same gate for bitmap OBJ OAM-alpha blending.

Targeted verification:

```sh
cargo test -p nds-core window_effects_disable --release
cargo test -p nds-core obj_brightness --release
cargo test -p nds-core gpu2d --release
cargo test -p nds-core gpu3d --release
```

Result:

- Window effects focused tests: `2` passed.
- OBJ brightness focused tests: `2` passed.
- GPU2D focused tests: `25` passed.
- GPU3D focused tests: `322` passed.

### Follow-up: 8bpp OBJ 2D mapping ignores the base tile low bit

Latest screenshots inspected:

```text
Screenshot 2026-06-07 at 12.52.50 AM.png
Screenshot 2026-06-07 at 12.53.38 AM.png
```

Observed behavior:

- The corruption is still on the bottom 2D scene, not the top 3D scene.
- The artifact is black, horizontally striped, and sprite-shaped near the
  player/stairs/furniture area.
- That shape is consistent with OBJ tile data being read half a tile out of
  phase, rather than a 3D polygon raster failure.

GBATEK notes that in 256-color OBJ mode only every second tile may be used; in
2D mapping mode the low bit of the tile number is ignored. The previous OBJ
renderer used the raw OAM tile number in 2D 8bpp mode:

```text
addr = tile_num * 32 + tile_offset * 64
```

For an odd tile number, that starts the sprite at byte `+32`, the middle of an
8bpp 8x8 tile. That can turn transparent or unrelated tile bytes into visible
black garbage rows.

Fix:

- In 2D OBJ mapping with 256-color/8bpp OBJs, mask `tile_num & !1` before
  calculating the base tile address.
- Leave 1D mapping unchanged; GBATEK says odd tile numbers should not be used
  there, while 2D mapping explicitly ignores the low bit.

Focused coverage:

- `test_8bpp_2d_mapping_ignores_base_tile_low_bit` sets an odd tile base and
  proves the renderer samples byte `0` from the even-aligned 8bpp tile, not byte
  `32` from the old half-tile offset.

Targeted verification:

```sh
cargo test -p nds-core 8bpp_2d_mapping --release
cargo test -p nds-core gpu2d::obj --release
cargo test -p nds-core gpu2d --release
cargo test -p nds-core gpu3d --release
```

Result:

- 8bpp OBJ mapping focused test: `1` passed.
- OBJ focused tests: `11` passed.
- GPU2D focused tests: `26` passed.
- GPU3D focused tests: `322` passed.

### Follow-up: edge-marking must find a real edge to preserve opaque zero-dot AA

Status: **Tightened the anti-aliasing plus edge-marking zero-dot quirk**

GBATEK notes that anti-aliasing is accidentally applied to opaque 1-dot
polygons, line segments, and wireframes, making opaque 1-dot polygons disappear.
It also notes the edge-marking workaround only works when those primitives are
actually edge-marked, meaning their polygon ID differs from the framebuffer or
rear-plane ID at the pixel.

The previous AA pass kept opaque zero-dot pixels visible whenever global edge
marking was enabled. That was too broad: if the zero-dot polygon had the same
polygon ID as the rear plane, edge marking found no exposed edge, but the pixel
still survived the AA pass. This could leave small speckles in scenes that rely
on matching polygon IDs to suppress edge treatment.

Fix:

- Keep the existing behavior for globally-disabled edge marking: opaque
  zero-dot pixels are hidden by the AA quirk.
- When edge marking is enabled, preserve the zero-dot only if AA finds an
  exposed cross-edge neighbor.
- If no such edge exists, clear the pixel alpha bit and alpha buffer just like
  the non-edge-marked zero-dot path.

Focused coverage:

- `test_antialias_hides_zero_dot_when_edge_marking_finds_no_edge` renders an
  opaque zero-dot polygon whose ID matches the rear-plane ID with both AA and
  edge marking enabled, and verifies the pixel is hidden.
- Existing zero-dot coverage still verifies the opposite cases: a genuinely
  edge-marked zero-dot remains visible, and a translucent zero-dot remains
  visible because the opaque-AA quirk does not apply.

Targeted verification:

```sh
cargo test -p nds-core test_antialias_hides_zero_dot_when_edge_marking_finds_no_edge --release
cargo test -p nds-core antialias --release
cargo test -p nds-core gpu3d --release
```

Result:

- Zero-dot edge-mark regression test: `1` passed.
- Anti-alias focused tests: `22` passed.
- GPU3D focused tests: `323` passed.

### Follow-up: translucent fog depth follows the depth-update bit

Status: **Added focused fog/depth conformance coverage**

GBATEK says fog depth follows the value stored in the framebuffer depth buffer,
and for translucent polygons that value depends on `POLYGON_ATTR.Bit11`: when
the bit is clear the old depth remains, and when set the translucent fragment
writes its own depth. That means a translucent overlay can blend with the same
RGB color but fog differently depending on whether it updates depth.

The implementation already matched this behavior, but it was only indirectly
covered by separate depth-update and fog tests. This left a visual-conformance
regression risk for foggy translucent overlays: a future refactor could make
the color blend look right while applying fog from the wrong layer depth.

Focused coverage:

- `test_translucent_fog_depth_follows_depth_update_bit` draws a far,
  fog-enabled opaque base and a near, fog-enabled translucent overlay.
- With translucent depth update disabled, fog uses the far base depth and
  darkens the final pixel.
- With translucent depth update enabled, fog uses the near translucent depth
  and leaves the blended red pixel bright.

Targeted verification:

```sh
cargo test -p nds-core test_translucent_fog_depth_follows_depth_update_bit --release
```

Result:

- Translucent fog/depth focused test: `1` passed.

### Follow-up: edge marking uses the current post-translucent depth buffer

Status: **Added focused edge-marking/depth conformance coverage**

GBATEK notes that edge marking is applied after opaque and translucent polygons
have been rendered. That means a translucent polygon that updates depth can
change the depth values later used by the edge-marking post-pass. GBATEK calls
this a source of edge-marking problems, but the hardware-visible invariant is
clear: edge marking compares against the current depth buffer, not a saved
opaque-only depth snapshot.

The implementation already used the current depth buffer. The risk was that
future cleanup could make edge marking use stale opaque depth while preserving
the existing edge flag through a translucent overlay.

Focused coverage:

- `test_edge_marking_uses_current_depth_after_translucent_depth_update` seeds a
  flagged opaque edge beside a different-ID neighbor.
- With the old farther center depth, edge marking rejects the edge because the
  center is behind the neighbor.
- With a nearer center depth, modeling a depth-updating translucent overlay,
  the same flagged edge is marked with `EDGE_COLOR`.

Targeted verification:

```sh
cargo test -p nds-core test_edge_marking_uses_current_depth_after_translucent_depth_update --release
```

Result:

- Edge-marking current-depth focused test: `1` passed.

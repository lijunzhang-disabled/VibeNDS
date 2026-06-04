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

### Tests added

```text
test_bgr555_to_rgb888_expands_channels
test_capture_ppm_layout_size
test_capture_args_accept_sequence_options
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

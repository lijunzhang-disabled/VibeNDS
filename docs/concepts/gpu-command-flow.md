# Concept: GPU command flow (GXFIFO + dispatch)

Companion to `3d-graphics-basics.md` and `rasterization.md`. Those two cover *what* the GPU computes; this one covers *how* it gets asked to compute it.

The NDS 3D engine is fundamentally different from the 2D engines: it's **command-driven**, not register-driven. ARM9 software doesn't write to "the BG2 scroll register" or "the polygon-3 color register" — instead it pushes a stream of commands ("set this matrix," "submit this vertex," "swap buffers") into a hardware FIFO that the 3D engine consumes. This is the same pattern modern GPUs use, just 20 years older and smaller.

If you only read one section: §1 has the mental model; §4 has the command taxonomy; §5 traces one command end-to-end through our code.

## 1. Producer / consumer

```
                    GXFIFO (256-entry × 32-bit)
                    │
    ARM9 software   │   3D engine hardware
   ───────────────  │   ────────────────────
    builds          │   pops one command,
    a "command      │   feeds it through the
    list" in        │   pipeline (matrix stacks,
    main RAM,       │   vertex assembly, lighting,
    DMA's it        │   clipping, viewport),
    into ──────────►│──► then waits for the next
    GXFIFO          │   command.
   ───────────────  │   ────────────────────
       producer            consumer
```

ARM9's job is "build a list of commands describing this frame." The 3D engine's job is "pop them and execute." The two run asynchronously and decouple via the 256-entry FIFO.

This is structurally different from the 2D engines. Engine A composites BG0-3 + OBJ from registers that ARM9 wrote ahead of time — no commands, no FIFO, just memory-mapped state sampled per-pixel during the scanline render. The 3D engine has *no* equivalent state-only path: every change to its internal state goes through a command.

Why this difference? The 3D pipeline is *stateful in a sequence-dependent way*. The matrix stack at the moment a `VTX_16` command arrives depends on every `MTX_*` command that came before. Polygon attributes are latched at `BEGIN_VTXS`; writes before or during an active list are staged until the next list begins. Sequencing commands through a FIFO makes that sequencing explicit; trying to express the same thing via memory-mapped registers would require either CPU stalls (write a vertex, wait for processing, write the next) or duplicating the FIFO logic in software.

## 2. The producer side

ARM9 software has two ways to submit commands. Both end up in the same FIFO.

### 2a. Packed format — `0x04000400`

One 32-bit word at this address packs **up to 4 command IDs**, one byte each, LSB-first. After the packed word, ARM9 writes parameter words for each command in declaration order.

```
ARM9 writes to 0x04000400:

  word 1: 0x12_15_10_11           ← packs 4 cmd bytes (LSB-first):
                                     0x11 = MTX_PUSH   (0 params)
                                     0x10 = MTX_MODE   (1 param)
                                     0x15 = MTX_IDENTITY (0 params)
                                     0x12 = MTX_POP    (1 param)

  word 2: 0x00000002              ← MTX_MODE's parameter (= mode 2)
  word 3: 0x00000005              ← MTX_POP's parameter (= pop 5 levels)
```

After all 3 words are consumed, the 3D engine has executed 4 commands in declaration order: `MTX_PUSH` (zero params, fires immediately), `MTX_MODE 2`, `MTX_IDENTITY` (zero params, fires when its declaration slot is reached), `MTX_POP 5`.

Padding with `0x00` is valid — null bytes in the packed-cmd word are silently skipped.
Invalid nonzero command bytes are ignored and consume no parameters; unlike
`0x00` padding, they do not terminate the remaining bytes in that packed word.
Zero-parameter commands still occupy FIFO entries, but they do not consume
parameter words. A following FIFO word can therefore be the next packed command
word. GBATEK's DMA-overkill note explicitly calls out repeated
`Packed(00151515h)` words as producing many `MTX_IDENTITY` commands.

### 2b. Direct ports — `0x04000440..0x040005FF`

Each command has its own dedicated address: `0x04000440 + (cmd - 0x10) * 4`. Writing to that address is equivalent to submitting that command. For multi-parameter commands, ARM9 keeps writing to the same address; the hardware accumulates parameter words until it has the right count.

```
ARM9 writes to 0x0400048C  ← VTX_16's direct port (2 params)

  word 1: 0x00100020   ← param 1: y=0x10, x=0x20
  word 2: 0x00000030   ← param 2: z=0x30 (+ pad)
```

VTX_16 now has all its params; the 3D engine executes it.

**Why two paths?** Packed format is denser for batches of small commands (4 cmds in 1 word vs 4 separate writes). Direct ports are simpler for software that doesn't want to compose packed words. In practice, games use packed format for vertex-heavy inner loops and direct ports for occasional setup commands.

### 2c. The FIFO buffer

The 256-entry × 32-bit FIFO sits between ARM9's writes and the 3D engine's consumer. Three "fill levels" matter:

- **Entry count** (`GXSTAT[16..24]`) — number of 40-bit command FIFO entries.
- **Full** (`GXSTAT[24] = 1`) — FIFO is full. This bit overlaps the top
  visible count bit because a saturated visible count is 256 entries.
- **Less-than-half** (`GXSTAT[25] = 1`) — fewer than 128 entries; **this is the DMA replenishment trigger**.
- **Empty** (`GXSTAT[26] = 1`) — no FIFO entries pending.
- **General busy** (`GXSTAT[27] = 1`) — geometry work is still pending or executing.

Software's job is to keep the FIFO between ~32 and ~200 entries: enough work
pending that the engine never starves, not so much that ARM9 spends time
waiting on a full FIFO. Real games do this via DMA — see §6.

On hardware, CPU writes to GXFIFO or command ports wait while the FIFO is full;
the command stream is not dropped. The emulator does not model the exact CPU
stall yet, so it preserves over-capacity writes in FIFO order and caps the
reported `GXSTAT` count at the hardware-visible maximum.

## 3. The consumer side

`Engine3d` (in `gpu3d/engine.rs`) drains the FIFO via `drain_fifo`:

```rust
pub fn drain_fifo(&mut self) {
    while let Some(op) = self.fifo.pop_op() {
        self.dispatch(op);
    }
}
```

`dispatch` is one big match over the ~50 commands, calling into the right subsystem (matrix stacks, vertex pipeline, lighting, etc):

```rust
match cmd {
    GxCmd::MtxMode      => self.stacks.set_mode(...),
    GxCmd::MtxLoad4x4   => self.stacks.load(Matrix::load_4x4(...)),
    GxCmd::MtxMult4x4   => self.stacks.mult(...),
    GxCmd::Color        => self.vertex.set_color(...),
    GxCmd::Vtx16        => self.submit_vertex(decode_vtx16(...)),
    GxCmd::BeginVtxs    => self.vertex.begin(PrimitiveType::from_bits(...)),
    GxCmd::DifAmb       => self.lighting.set_dif_amb(...),
    GxCmd::SwapBuffers  => self.swap_pending = true,
    // ... 40+ more arms
}
```

The `GxCmd::param_count` lookup (`gpu3d/command.rs`) tells the FIFO decoder how many parameter words each command takes, so it knows when a packed-format multi-command word has its parameters fully accumulated and the command is "ready" to dispatch.

## 4. GX command taxonomy

The ~50 GX opcodes fall into 6 categories. Knowing the categories is the fastest way to predict what any given opcode does.

### 4a. Matrix commands (`0x10..0x1C`, 13 commands)

Update one of the 4 matrix stacks (projection / position / position+vector / texture).

| Op | Name | Params |
|---|---|---:|
| 0x10 | MTX_MODE | 1 |
| 0x11 | MTX_PUSH | 0 |
| 0x12 | MTX_POP | 1 |
| 0x13 | MTX_STORE | 1 |
| 0x14 | MTX_RESTORE | 1 |
| 0x15 | MTX_IDENTITY | 0 |
| 0x16 | MTX_LOAD_4x4 | 16 |
| 0x17 | MTX_LOAD_4x3 | 12 |
| 0x18 | MTX_MULT_4x4 | 16 |
| 0x19 | MTX_MULT_4x3 | 12 |
| 0x1A | MTX_MULT_3x3 | 9 |
| 0x1B | MTX_SCALE | 3 |
| 0x1C | MTX_TRANS | 3 |

### 4b. Vertex attribute commands (`0x20..0x2C`, 12 commands)

Either submit a vertex position (which triggers transformation + assembly) or set an attribute that applies to the *next* vertex.

| Op | Name | Params | Affects |
|---|---|---:|---|
| 0x20 | COLOR | 1 | next vertex's color |
| 0x21 | NORMAL | 1 | next vertex's lit color (triggers lighting) |
| 0x22 | TEXCOORD | 1 | next vertex's UV |
| 0x23 | VTX_16 | 2 | submit vertex (16-bit per component) |
| 0x24 | VTX_10 | 1 | submit vertex (10-bit packed) |
| 0x25-27 | VTX_XY/XZ/YZ | 1 | submit vertex (2 components, third kept) |
| 0x28 | VTX_DIFF | 1 | submit vertex (delta from previous) |
| 0x29 | POLYGON_ATTR | 1 | polygon flags latched by the next `BEGIN_VTXS` |
| 0x2A | TEXIMAGE_PARAM | 1 | texture format + VRAM offset for upcoming vertices |
| 0x2B | PLTT_BASE | 1 | texture palette base for upcoming vertices |

### 4c. Lighting / material (`0x30..0x34`, 5 commands)

Set per-light or per-material state that affects subsequent vertex lighting.

| Op | Name | Params |
|---|---|---:|
| 0x30 | DIF_AMB | 1 |
| 0x31 | SPE_EMI | 1 |
| 0x32 | LIGHT_VECTOR | 1 |
| 0x33 | LIGHT_COLOR | 1 |
| 0x34 | SHININESS | 32 |

### 4d. Geometry control (`0x40..0x60`, 4 commands)

Bracket primitives + frame-level events.

| Op | Name | Params |
|---|---|---:|
| 0x40 | BEGIN_VTXS | 1 |
| 0x41 | END_VTXS | 0 |
| 0x50 | SWAP_BUFFERS | 1 |
| 0x60 | VIEWPORT | 1 |

`BEGIN_VTXS` starts or restarts the active vertex list. `END_VTXS` exists in
the command table but behaves as a no-op on real hardware; lists are really
terminated by the next `BEGIN_VTXS` or by `SWAP_BUFFERS`.

### 4e. Test commands (`0x70..0x72`, 3 commands)

Hardware-accelerated frustum / position / vector tests; results go to `GXSTAT`. Used for ARM9-side culling.

| Op | Name | Params |
|---|---|---:|
| 0x70 | BOX_TEST | 3 |
| 0x71 | POS_TEST | 2 |
| 0x72 | VEC_TEST | 1 |

`VEC_TEST` uses the directional/vector matrix and must be issued with
`MTX_MODE = 2` selected. `POS_TEST` also updates the inherited vertex-position
state, so a following relative vertex command will be relative to the tested
position.

That's ~50 commands. Every one falls into one of six buckets: "update a matrix," "set a vertex attribute," "submit a vertex," "set lighting/material," "bracket a primitive group," or "test." The whole 3D engine is administrative wiring around per-vertex matrix algebra.

## 5. A single command's journey

To make the flow concrete, here's the full path through our code when ARM9 executes one instruction that says "load a translation matrix":

```
ARM9 instruction:  STR R0, [R1]           where R0 = packed (tx, ty, tz)
                                                R1 = 0x04000470  (MTX_TRANS direct port)

  ↓ ARM9's cpu.write32 dispatch
  ↓ bus/arm9.rs::write32 sees addr >> 24 == 0x04
  ↓ calls bus/io_arm9.rs::write_io32
  ↓ matches 0x0440..0x0600 range → direct port handler
  ↓ decodes cmd_byte = (0x470 - 0x440) / 4 + 0x10 = 0x1C = MTX_TRANS
  ↓ shared.gpu3d.fifo.write_direct(MTX_TRANS, val)
  ↓
gpu3d/fifo.rs::GxFifo::write_direct:
  ↓ MTX_TRANS needs 3 params (per command::param_count)
  ↓ first param word stored in pending_cmds queue
  ↓ ARM9 writes 2 more times → params complete (lined up via the
  ↓   same direct-port address, accumulating into the same pending entry)
  ↓ GxOp { cmd: 0x1C, params: [tx, ty, tz] } pushed to ready queue
  ↓
shared.gpu3d.drain_fifo()  (called inline from write_io32):
  ↓ pop GxOp from ready
  ↓ dispatch(op) matches GxCmd::MtxTrans
  ↓ current matrix ← current × T(tx, ty, tz)  via stacks.load(current.mul_translate(...))
  ↓
End state:
  shared.gpu3d.stacks.position (or projection/etc per current mode)
  now holds the post-translate matrix. The next VTX_16/10/etc command
  will transform vertices using this new value.
```

That whole journey spans ~5 function calls across 4 modules (`bus/io_arm9.rs` → `gpu3d/fifo.rs` → `gpu3d/engine.rs` → `gpu3d/stacks.rs` → `gpu3d/matrix.rs`). The same path applies to every command in the GX set, with different terminal subsystems.

## 6. GXFIFO replenishment via DMA

For a typical commercial frame, a game submits thousands of commands per frame. Pushing them all via individual `STR` instructions would burn CPU cycles and saturate the bus. Instead games **precompute a command list in main RAM and DMA it to the GXFIFO**.

The hardware loop:

```
                                                   ┌────────────────────────┐
   Game code:                                       │  Main RAM command list │
                                                   │  (precomputed, hundreds│
                                                   │   to thousands of      │
                                                   │   words per frame)     │
                                                   └───────────┬────────────┘
                                                               │
   1. Configure DMA9 channel: src=cmdlist, dst=GXFIFO,         │
      timing=GXFIFO-half-empty, count=N words                  │
                                                               │
   2. ARM9 goes off to do other work                            │
                                                               │
   3. As the 3D engine drains commands, GXFIFO drops           │
      below 128 entries                                         │
                                                               │
   4. Hardware sees the half-empty edge and fires the DMA      │
      → DMA copies a burst from main RAM into GXFIFO ──────────┘
                                                               │
   5. 3D engine keeps consuming; loop repeats until            │
      DMA count exhausted                                       │
```

Without this, software would have to babysit the FIFO (poll `GXSTAT`, push more, repeat). With it, ARM9 sets up one DMA and is free to compute the *next* frame's command list while frame N drains.

This is the **Phase 4 carry-over** that landed in Phase 6: when the FIFO's `fell_below_half_edge` flag flips, `Bus9::write32` returns `Write32Effect::FireGxFifoDma`, which fires every ARM9 DMA channel armed for `DmaTiming::GxFifo`. (`gpu3d/fifo.rs::take_below_half_edge`, `bus/arm9.rs::write32`.)

## 7. SWAP_BUFFERS as the synchronization point

`SWAP_BUFFERS` (cmd `0x50`) is the only command with frame-boundary semantics. When it fires:

- The 3D engine sets `swap_pending = true`.
- At the next VBlank-end (line 0 transition), the top-level scheduler calls `engine.swap_buffers(...)`.
- That moves `geometry_polygons` → `raster_polygons` and rasterizes immediately into the framebuffer.
- The framebuffer becomes Engine A's BG0 source for the new frame.

In other words, **`SWAP_BUFFERS` is the NDS's `Present`**. It marks "frame N's command stream is done; render and display it." Until `SWAP_BUFFERS` arrives, the engine keeps building geometry into `geometry_polygons`; after, the buffer flip happens at the next frame boundary.

If ARM9 submits commands *without* a `SWAP_BUFFERS`, the polygons sit in `geometry_polygons` forever and the display never updates with the new geometry. (Engine A still composites the old `raster_polygons` content.)

## 8. What "the GPU" actually is in our emulator

Concretely, the 3D engine is the `Engine3d` struct in `gpu3d/engine.rs`. It owns:

| Field | What it tracks |
|---|---|
| `fifo: GxFifo` | the 256-entry command queue + decoder state |
| `stacks: MatrixStacks` | 4 matrix stacks (projection, position, vector, texture) |
| `vertex: VertexState` | current color/normal/tex, primitive type, accumulated vertices |
| `lighting: LightingState` | 4 lights + materials + shininess LUT |
| `viewport: Viewport` | screen rect from VIEWPORT command |
| `geometry_polygons: Vec<ScreenPolygon>` | this frame's polygons under construction |
| `raster_polygons: Vec<ScreenPolygon>` | last frame's polygons (input to rasterizer) |
| `rasterizer: Rasterizer` | 256×192 framebuffer + post-effects |

Each command modifies exactly one of those fields. `BEGIN_VTXS` flips `vertex.primitive`. `VTX_16` transforms a vertex through `stacks.clip_matrix()` and appends to `vertex.vertex_buffer`. `SWAP_BUFFERS` flips `swap_pending = true`. Etc.

The full set of "what a 3D engine is" reduces to: state fields + a command dispatcher that mutates them.

## 9. Modern GPUs work this way too

The producer/consumer command-bus pattern is **the standard GPU architecture** today. D3D12, Vulkan, Metal — all three are explicit command-list APIs:

| Concept | NDS 3D engine | Modern GPU (D3D12 / Vulkan / Metal) |
|---|---|---|
| Producer | ARM9 software | CPU application code |
| Command queue | GXFIFO (256 × 32 bit) | command queue (per-thread, hardware-backed) |
| Command unit | GX opcode (e.g. `MTX_TRANS`) | command list entry (e.g. `vkCmdDraw`) |
| State updates | `MTX_LOAD`, `COLOR`, etc | pipeline state objects, descriptor sets |
| Vertex submission | `VTX_16` etc | vertex buffer + `vkCmdDraw` |
| Frame finalization | `SWAP_BUFFERS` | `vkQueuePresent` (or D3D12 `Present`) |
| DMA replenishment | DMA9 GxFifo timing | DMA built into the GPU's command-fetch unit |

The NDS GPU is a fixed-function command-list machine; modern GPUs are programmable command-list machines (you ship shaders alongside commands). Same fundamental architecture, ~20 years apart.

Why the older 2D engines on NDS *don't* work this way: per-pixel composition has a fixed cost dominated by the per-scanline state. Sampling state from registers per-scanline costs nothing extra. Streaming "set background color, render scanline 0, set background color, render scanline 1, …" would just add overhead. The 3D engine's per-polygon work, by contrast, is unbounded in complexity per frame — geometry counts vary by 100× across a single game — so the command-list model wins.

## 10. The mental model

> **The 3D engine is a small CPU that runs a tiny instruction set (~50 opcodes). ARM9 writes "programs" for that CPU into a 256-entry FIFO. The 3D engine pops one opcode at a time, updates its internal state (matrix stacks, vertex pipeline, lighting, etc), and produces polygons. SWAP_BUFFERS is the "yield" instruction that hands the frame's output to the rasterizer.**

Every commit so far in `nds-core/src/gpu3d/` is a direct realization of that model:

- `command.rs` is the instruction-set definition.
- `fifo.rs` is the instruction queue.
- `engine.rs::dispatch` is the instruction executor.
- `stacks.rs`, `vertex.rs`, `lighting.rs` are the "registers" the instructions read and write.
- `raster/` is the output stage that runs at SWAP_BUFFERS.

Read those files in that order and the whole 3D engine is ~1700 lines of "command-driven state machine."

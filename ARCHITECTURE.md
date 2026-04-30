# NDS Emulator — Technical Architecture

## High-Level Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                            Nds (top-level)                            │
│                                                                       │
│  ┌──────────┐                              ┌──────────┐               │
│  │  Cpu9    │──── borrows Bus9 ───────────>│   Bus9    │              │
│  │ ARM946E-S│                              │  (ARM9    │              │
│  │ + CP15   │                              │   side)   │              │
│  └──────────┘                              └─────┬────┘               │
│       │                                          │                    │
│  ┌──────────┐                              ┌─────▼────┐               │
│  │  Cpu7    │──── borrows Bus7 ───────────>│   Bus7    │              │
│  │ ARM7TDMI │                              │  (ARM7    │              │
│  └──────────┘                              │   side)   │              │
│                                            └─────┬────┘               │
│                                                  │                    │
│                                          ┌───────▼─────────┐          │
│                                          │   SharedState   │          │
│                                          │  ┌────────────┐ │          │
│                                          │  │  Main RAM  │ │          │
│                                          │  │   4 MB     │ │          │
│                                          │  └────────────┘ │          │
│                                          │  ┌────────────┐ │          │
│                                          │  │ Shared WRAM│ │          │
│                                          │  │   32 KB    │ │          │
│                                          │  └────────────┘ │          │
│                                          │  ┌────────────┐ │          │
│                                          │  │  VRAM A-I  │ │          │
│                                          │  │  656 KB    │ │          │
│                                          │  └────────────┘ │          │
│                                          │  ┌────────────┐ │          │
│                                          │  │  Palette   │ │          │
│                                          │  │  OAM, IPC  │ │          │
│                                          │  └────────────┘ │          │
│                                          └─────────────────┘          │
│                                                                       │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐               │
│  │  Gpu2d_A │  │  Gpu2d_B │  │  Gpu3d   │  │  Audio   │               │
│  │ (full)   │  │ (subset) │  │ (geom +  │  │  16 ch   │               │
│  │          │  │          │  │  raster) │  │          │               │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘               │
│                                                                       │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐               │
│  │ Dma9 ×4  │  │ Dma7 ×4  │  │ Tmr9 ×4  │  │ Tmr7 ×4  │               │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘               │
│                                                                       │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐               │
│  │  Cart    │  │  SPI bus │  │   IRQ9   │  │   IRQ7   │               │
│  │ slot-1   │  │ Firmware/│  │          │  │          │               │
│  │ (KEY1/2) │  │ TSC/PMIC │  │          │  │          │               │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘               │
│                                                                       │
│  ┌──────────┐  ┌────────────────────────────────────────┐             │
│  │Scheduler │  │     framebuffer_top, framebuffer_bot   │             │
│  │ min-heap │  │     (256×192 each, BGR555)             │             │
│  └──────────┘  └────────────────────────────────────────┘             │
└───────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
                         ┌─────────────────────┐
                         │   SDL2 frontend     │
                         │ (dual-screen video, │
                         │  audio ring buffer, │
                         │  keyboard + touch)  │
                         └─────────────────────┘
```

## Ownership Model — Borrow-Checker Plan

The GBA's "sibling fields" pattern was sufficient because there's one CPU and one bus. NDS has two CPUs that both touch shared memory and many shared peripherals (palette, VRAM, OAM, IPC, IRQ flag registers, GPU state). We extend the pattern with a **`SharedState` field** that both buses can dip into:

```rust
pub struct Nds {
    pub cpu9: Cpu,            // ARM9 (is_arm9 = true, has CP15)
    pub cpu7: Cpu,            // ARM7
    pub bus9: Bus9,           // ARM9-only memories: ITCM/DTCM, ARM9 BIOS
    pub bus7: Bus7,           // ARM7-only memories: ARM7 BIOS, ARM7 WRAM
    pub shared: SharedState,  // Main RAM, shared WRAM, VRAM, palette, OAM, IPC
    pub gpu2d_a: Engine2d,    // Engine A
    pub gpu2d_b: Engine2d,    // Engine B
    pub gpu3d: Gpu3d,
    pub audio: Audio,
    pub dma9: Dma9, pub dma7: Dma7,
    pub tmr9: Timers, pub tmr7: Timers,
    pub irq9: Irq, pub irq7: Irq,
    pub cart: Cart,
    pub spi: SpiBus,
    pub scheduler: Scheduler,
    framebuffer_top: Vec<u16>,  // 256×192
    framebuffer_bot: Vec<u16>,
}
```

### How the CPU step is borrow-safe

A CPU's `step()` needs `&mut Bus9` (or `&mut Bus7`). The bus is constructed on demand by *gluing* the per-CPU memory and a mutable borrow of `SharedState` together. Two flavors:

**Flavor A — pre-built bus structs (preferred for ARM9):**
`Bus9` owns its private memories (ITCM / DTCM / ARM9 BIOS) directly. For shared regions it carries a `&'a mut SharedState` (lifetime-parameterized). The top-level loop builds it per chunk:

```rust
let mut bus9 = Bus9::view(&mut self.cpu9_mem, &mut self.shared, &mut self.gpu2d_a, ...);
let cycles = self.cpu9.step(&mut bus9);
```

This is the same shape as GBA's `Bus`, just with extra borrowed fields. The catch: every cross-cutting call site (e.g. ARM9 writes to `IPCFIFOSEND`, ARM7 needs to wake) must avoid touching the *other* CPU's mutable state during the same call. We resolve this via the **pending-event pattern** below.

**Flavor B — pending-event pattern (cross-CPU side effects):**
Just like GBA's `pending_swi`, each CPU sets a flag/queue when an action will affect the other CPU; the top-level loop drains them between steps:

```rust
// Inside ARM9 step:
self.shared.ipc.fifo_9to7.push(value);
self.shared.pending_irq7 |= IrqBits::IPC_RECV;

// Top loop after the step:
self.irq7.raise(self.shared.pending_irq7.take());
```

The same trick handles GXFIFO half-empty (ARM9 writes set a flag → DMA9 channel kicked at chunk boundary), AUXSPI completion (ARM7 SPI transfer flag → IRQ7), display capture (Engine A finishes a frame → write captured framebuffer to a VRAM bank).

### Rationale: why not `Rc<RefCell<…>>` for everything

We could put `SharedState` inside `Rc<RefCell>` and side-step lifetime gymnastics. We don't, because:
1. The GBA project's borrow discipline is one of the cleanest parts of the codebase; we want to preserve it.
2. Save states need fast `bincode::serialize(&self)` round-trips. `RefCell` works but `Rc` cycles complicate it.
3. The bus build cost is zero with `&mut`; with `Rc<RefCell>` every load/store becomes a borrow check.

## CPU Architecture: ARM946E-S vs ARM7TDMI

### Shared baseline (ARMv4T)

Both cores execute the ARMv4T base set: data processing, multiply, MSR/MRS, single + halfword + block transfer, branch, BX, SWI. The same `Cpu`, `Alu`, `Arm`, `Thumb` modules port verbatim from `../gba`.

### ARM946E-S additions (ARMv5TE)

Gated behind `cpu.is_arm9`. New encodings handled by adjusting the existing dispatch:

| Mnemonic | Encoding hint | What it does |
|---|---|---|
| `CLZ Rd, Rm` | `cond 0001 0110 1111 Rd 1111 0001 Rm` | Count leading zeros |
| `BLX <imm>` | `1111 101H imm24` (top nibble = `0xF`, condition AL only) | Branch with link, switch to THUMB |
| `BLX Rm` | `cond 0001 0010 1111 1111 1111 0011 Rm` | Branch with link, mode from Rm[0] |
| `BX Rm` | (already in v4T) | THUMB now also has `BX` and v5T `BLX Rm` |
| `QADD/QSUB/QDADD/QDSUB` | `cond 0001 00x0 Rn Rd 0000 0101 Rm` | Saturating arithmetic |
| `SMLA<x><y>`, `SMLAW<y>`, `SMLAL<x><y>`, `SMUL<x><y>`, `SMULW<y>` | `cond 0001 0xx0 Rd Rn Rs 1yx0 Rm` | DSP multiply (v5TE-DSP) |
| `LDRD/STRD Rd, [Rn,...]` | halfword-class with bit 22=0, bit 5=1, bit 6=0/1 | 64-bit load/store (Rd, Rd+1) |
| `MCR/MRC/CDP/MCRR/MRRC` | `cond 1110 …` | Coprocessor ops (only CP15 exists on NDS) |
| `PLD` | `1111 01x1 U101 Rn 1111 imm12` | Cache hint, NOP for us |

The THUMB additions (BLX, register BLX) hook into the existing `0x47xx` decode block.

### CP15 — System Control Coprocessor (ARM9 only)

CP15 is read/written via `MCR`/`MRC`. We model the registers we care about:

| CRn | Description | What we model |
|---|---|---|
| c0 | ID + cache type | Read returns hardcoded ARM946E-S IDs |
| c1 | Control: `[12]=I-cache, [2]=D-cache, [13]=high vector base (=1 on NDS), [0]=MPU enable` | Bit 13 latches the BIOS vector base to 0xFFFF0000 |
| c2 | Cacheable / non-cacheable region bits | Stored, not enforced |
| c3 | Write-buffer region bits | Stored, not enforced |
| c5 | Access permissions (D-perms low/high, I-perms low/high) | Stored, MPU stub |
| c6 | MPU region base+size (8 regions) | Stored; full enforcement deferred to Phase 9 |
| c7 | Cache control: clean, invalidate, drain, prefetch flush | NOPs (we don't simulate cache contents) |
| c9 | Cache lockdown + **TCM region** (D-TCM at op2=0, I-TCM at op2=1): `[31:12]=base, [5:1]=size_field` | Drives `dtcm_base/size` and `itcm_size`; ITCM is always at 0 in our impl |

NDS BIOS sets:
- ITCM: 32 KB (size_field = 5), based at 0x00000000, mirrors fill the 32 MB window
- DTCM: 16 KB (size_field = 4), based at 0x027C0000 in some BIOS revs / 0x0B000000 in others — game-controlled

We honor whatever the BIOS / loader writes; we don't hardcode.

## Dual-CPU Bus & Memory Routing

### Address decode (ARM9)

```
addr >> 24:
  0x00              → ITCM (if ITCM size > 0 and addr < itcm_size, mirrored)
  0x02              → Main RAM
  0x03              → Shared WRAM (if WRAMCNT mode allocates this side; else open bus)
  0x04              → I/O page (ARM9 view)
  0x05              → Palette RAM (1 KB Engine A at +0, 1 KB Engine B at +0x400)
  0x06              → VRAM (lookup via VRAMCNT routing)
  0x07              → OAM (1 KB Engine A at +0, 1 KB Engine B at +0x400)
  0x08, 0x09        → GBA slot-2 ROM (only when EXMEMCNT bit 7 = 0)
  0x0A              → GBA slot-2 SRAM (only when EXMEMCNT bit 7 = 0)
  0xFF              → ARM9 BIOS (only at 0xFFFF0000+, otherwise open bus)

DTCM check: if addr is in [dtcm_base, dtcm_base + dtcm_size), DTCM wins over the
above; we test it as the FIRST decode step.
```

### Address decode (ARM7)

```
addr >> 24:
  0x00              → ARM7 BIOS (16 KB, mirrored)
  0x02              → Main RAM
  0x03              → Shared WRAM (per WRAMCNT) → fall through to ARM7 WRAM (64 KB) if not mapped
  0x04              → I/O page (ARM7 view)
  0x06              → VRAM banks routed to ARM7 (banks C/D, when WRAMCNT bits route them)
  0x08, 0x09        → GBA slot-2 ROM (only when EXMEMCNT bit 7 = 1, the default)
  0x0A              → GBA slot-2 SRAM
```

### Shared WRAM (`WRAMCNT` at 0x04000247)

| WRAMCNT | ARM9 sees | ARM7 sees |
|---|---|---|
| 0 | All 32 KB | None (open bus) |
| 1 | Upper 16 KB | Lower 16 KB |
| 2 | Lower 16 KB | Upper 16 KB |
| 3 | None (open bus) | All 32 KB |

Implementation: `SharedState::wram` is `[u8; 32*1024]`; `Bus9::wram_lookup(addr)` and `Bus7::wram_lookup(addr)` return `Option<&[u8]>` based on `WRAMCNT`.

### VRAM Routing

The most intricate piece of NDS memory. Each bank A-I has a `VRAMCNT_x` register:

```
VRAMCNT_x byte:
  [7]   enable
  [6:5] offset (0..3) — applies in some MST modes
  [2:0] mst (mode select) — interpreted differently per bank
```

Per bank, `(mst, offset)` map to a target. Examples for bank A (mst values):
- 0: LCDC (visible at 0x06800000+0x00000)
- 1: Engine A BG slot 0..3 (offset selects which 128 KB slot of the 512 KB engine-A-BG window)
- 2: Engine A OBJ slot 0..1
- 3: Texture image slot 0..3
- (bank A doesn't support more)

We model this as a **VRAM router**:

```rust
pub struct VramRouter {
    targets: [VramTarget; NUM_TARGETS],   // each target lists which banks back it
}

pub enum VramTarget {
    LcdcBank(BankId),       // direct bank-by-bank for LCDC display mode
    EngineABg(u32),         // 512 KB window, populated by 0..N banks
    EngineAObj(u32),        // 256 KB
    EngineBBg(u32),         // 128 KB
    EngineBObj(u32),        // 128 KB
    TextureImage(u32),      // 512 KB
    TexturePalette(u32),    // 128 KB
    BgExtPalA(u32),         // 32 KB (Engine A)
    ObjExtPalA(u32),        // 8 KB
    BgExtPalB(u32),         // 32 KB (Engine B)
    ObjExtPalB(u32),        // 8 KB
    Arm7(u32),              // 256 KB (banks C/D only)
}
```

On every `VRAMCNT_x` write, we recompute the routing tables. CPU reads at `0x06xxxxxx` consult these tables. The PPU and 3D texture unit read via the same router.

When two banks overlap in the same target (legal — writes go to all, reads return the OR or panic in dev mode), our router maintains a list per target.

## 2D Engine Architecture (per engine)

```
Engine A or B:
  ├── DISPCNT (mode select, BG/OBJ enable, display mode)
  ├── BG0CNT..BG3CNT (priority, base, size, color depth, ext-pal slot)
  ├── BG0HOFS/VOFS..BG3HOFS/VOFS
  ├── BG2/3 affine: PA/PB/PC/PD + reference points X/Y
  ├── WIN0H/WIN1H, WIN0V/WIN1V, WININ, WINOUT
  ├── MOSAIC, BLDCNT, BLDALPHA, BLDY
  └── MASTER_BRIGHT
```

### Display modes (DISPCNT bits 16-17, ARM9 only — ARM7 doesn't have a 2D engine)

| Mode | What's displayed |
|---|---|
| 0 | Display off (white) |
| 1 | Normal compositing (BGs + OBJ + 3D layer if Engine A) |
| 2 | Direct VRAM display: Engine A reads a single VRAM bank as a framebuffer |
| 3 | Main Memory display: ARM9 main RAM as a framebuffer streamed via DMA channel 3 (Display mode) |

### BG modes per engine (DISPCNT bits 0-2)

Same as GBA modes 0-2 plus:
- Mode 3-5: extra mixes of 1 affine + 2 text
- Mode 6 (Engine A only): "large screen" — single 512×1024 affine BG using BG2 only

### Compositing pipeline (per scanline)

```
For each of 256 pixels:
  1. Determine window region → WindowFlags(per-layer enable + effects enable)
  2. Collect candidate pixels: BG0..BG3 (mode-dependent), OBJ, 3D (Engine A only,
     DISPCNT bit 3 + BG0 source replaced)
  3. Sort by priority (lower = on top); ties broken: 3D > OBJ > BG0..BG3
  4. Top + Second pixel selected
  5. Apply blending (alpha / brightness up / down) per BLDCNT
  6. Apply MASTER_BRIGHT (post-blend brightness scale)
  7. Write to framebuffer_top or framebuffer_bot per POWCNT1.lcd_swap
```

### Extended palette (256-color tile BGs, OBJs)

When `DISPCNT.bg_extpal` is set, BG tile palette index (8-bit) selects from 16 × 256-color sub-palettes, where the sub-palette index comes from the tile-map entry's high bits. The palette source is whichever VRAM bank is routed to "Engine X BG ext-pal" target. Same mechanism for OBJ.

## 3D Engine Architecture

```
ARM9 issues GX commands
    │
    ▼
GXFIFO  ──  256-entry × 32-bit FIFO  ──> Command parser
    │
    ▼
Geometry stage:
  ├── Matrix stack manager (proj 1, pos 32, vector 32, tex 1)
  ├── MTX_MUL_xx  → updates current matrices
  ├── BEGIN_VTXS  → starts a primitive (tri/quad/tri-strip/quad-strip)
  ├── VTX_xx      → submit vertex (transformed by clip = proj*pos*vtx)
  ├── COLOR/NORMAL/TEXCOORD → vertex attributes
  ├── Lighting (4 dirs, ambient/diffuse/specular)
  ├── Polygon assembly (3 or 4 vertices → 1 polygon)
  ├── Sutherland-Hodgman clip vs ±W on 6 planes
  └── Viewport / perspective divide → screen-space polygon list
    │
    ▼
SWAP_BUFFERS (cmd 0x50)  →  flush polygon list to render buffer
    │
    ▼
Rasterizer (runs per scanline during frame N+1):
  ├── Sort by Y, fetch all polygons covering scanline
  ├── For each polygon span:
  │     ├── Interpolate Z/W, color RGBA, U/V, fog factor
  │     ├── Texture fetch (8 formats; palettes from "texture palette" target)
  │     ├── Depth test (Z or W per DISP3DCNT)
  │     ├── Alpha test (threshold from DISP3DCNT)
  │     ├── Blend (alpha) or write (opaque)
  │     ├── Toon / highlight / fog post-effects
  │     └── Write color to 3D framebuffer + Z to depth buffer
  └── Edge marking pass (after polygon pass): tint pixels at polygon-ID edges
    │
    ▼
3D framebuffer (256×192)  ──>  Engine A (DISPCNT bit 3 enables 3D-as-BG0)
```

### Matrix math precision

NDS uses 1.19.12 fixed-point for transformation matrices. We use `i32` per element and `i64` for intermediate products. The full transformation per vertex is:

```
clip = proj * pos * vertex
       (1.19.12)  (1.19.12)  (1.3.12)  → (1.3.12)
                                       → divide by W → (1.0.12) NDC
                                       → viewport → screen pixel
```

Lighting works in pre-projection space; texture coords run through the texture matrix in parallel.

### Polygon RAM and vertex RAM

Two double-buffered banks (frame N writes, frame N+1 reads):
- Polygon RAM: 2048 entries
- Vertex RAM: 6144 entries

`SWAP_BUFFERS` swaps the banks at the start of the next frame. If the geometry stage overflows during a frame, subsequent polygons are dropped (and `GXSTAT.overflow` is set).

## DMA Architecture

### ARM9 DMA (4 channels, `DMA0..3` at 0x040000B0..)

Start modes (`DMAxCNT_H[13:11]`):

| Mode | Trigger |
|---|---|
| 0 | Immediate (on enable rising edge) |
| 1 | VBlank |
| 2 | HBlank (only fires for visible scanlines 0..191) |
| 3 | Display sync (every scanline at start of HDraw) |
| 4 | Main memory display FIFO (DMA3 only, for capture mode 3) |
| 5 | Slot-1 cart |
| 6 | Slot-2 cart |
| 7 | GXFIFO half-empty (writes to GXFIFO drain into the geometry pipeline) |

### ARM7 DMA (4 channels)

Start modes (`DMAxCNT_H[13:12]` — only 2 bits on ARM7):

| Mode | Trigger |
|---|---|
| 0 | Immediate |
| 1 | VBlank |
| 2 | Slot-1 cart |
| 3 | Wireless / channel-specific (channels 1/3 only — sound on others) |

Channels 1, 2, 3 also have hardcoded "sound" semantics when configured to read from a sound channel's source pointer; the sound mixer pulls samples directly via the bus, so this is a labeling distinction more than a routing one.

### Borrow-checker notes

DMA executes as a method on the relevant bus, like GBA: `Bus9::run_dma(channel)` reads the channel's src/dst, performs `read32`/`write32` against its own bus, updates state. Cross-CPU DMA scenarios don't exist (each CPU only DMAs against its own bus), but DMA *can* read VRAM that's currently mapped to the other side or to a 2D engine — the VRAM router enforces consistency.

## Inter-Processor Communication

### IPCSYNC (0x04000180, mirrored on both buses)

```
[3:0]   data input from the OTHER CPU (read-only)
[11:8]  data output to the OTHER CPU (write)
[13]    "send IRQ to other CPU on output change" (write-only trigger)
[14]    "enable receive IRQ" (when other CPU sets bit 13 with this set, raise IRQ)
```

Both buses see the same IPCSYNC register; the two CPUs read each other's halves.

### IPCFIFO (0x04000184 / 0x04000188)

Two FIFOs (16 × 32-bit each), one per direction. `IPCFIFOSEND` (0x04000188) writes from this CPU into the *send* FIFO; `IPCFIFORECV` (0x04100000 — yes, the address is in the 0x04100000 page) reads from the *receive* FIFO.

`IPCFIFOCNT` (0x04000184):
- `[0]`  send FIFO empty (RO)
- `[1]`  send FIFO full (RO)
- `[2]`  send FIFO empty IRQ enable
- `[3]`  send FIFO clear (write 1 to clear send FIFO)
- `[8]`  recv FIFO empty (RO)
- `[9]`  recv FIFO full (RO)
- `[10]` recv FIFO not-empty IRQ enable
- `[14]` error flag (read empty / write full happened)
- `[15]` enable

Implementation: `SharedState::ipc.fifo_9to7: VecDeque<u32>` and `fifo_7to9: VecDeque<u32>`. ARM9 `SEND` push to `fifo_9to7`; ARM7 `RECV` pop from `fifo_9to7`. Read on empty: returns last-popped value (or 0), sets error flag. Write on full: drops the word, sets error flag.

## SPI Bus

SPI is shared between three devices (selected by `SPICNT.device_select`):

- Power management (PMIC, dev 0)
- Firmware (SPI flash, dev 1)
- Touchscreen (TSC, dev 2)

`SPICNT[15]` busy bit; `SPIDATA[7:0]` shifts a byte in/out. Hold (`SPICNT[11]`) keeps CS asserted across multiple bytes (needed for multi-byte transfers like firmware reads). Per-byte transfer takes ~64 ARM7 cycles depending on baud.

Each device implements `fn xfer(&mut self, byte_in: u8, hold: bool) -> u8` and tracks its own state machine.

### TSC (ADS7843-style)

Control byte format: `[7]=start, [6:4]=channel, [3]=mode (0=12-bit), [2]=ref, [1:0]=power`.

Channels we emulate: 1=Y, 2=Battery (returns ~80% of full scale), 3=Z1, 4=Z2, 5=X, 6=AUX (bottom screen brightness — not really, returns 0).

X/Y conversion to ADC values uses the firmware's calibration block (factory-set or our default).

### Firmware

We implement a minimal SPI flash command set. The firmware image we synthesize has:
- Header at offset 0 (revision, supported language flags)
- WiFi calibration block (offset 0x2A — empty/zero is fine for non-WiFi homebrew)
- User settings block twice (rotating; pick the higher counter)

If `--firmware` flag points at a real dump, we use it verbatim.

## Cartridge

### Slot-1 protocol

The cart talks to the system over the slot-1 bus, controlled by ARM9 (default) or ARM7 (per `EXMEMCNT[11]`). The protocol is request/response: ARM9 writes 8 bytes of "command" to the cart, optionally encrypted with KEY1; cart returns up to 0x4000 bytes streamed back via `ROMDATA` reads, optionally encrypted with KEY2.

Command set we emulate:
- `0x9F` (dummy) — return 0xFFs
- `0x00` (read header) — return first 0x200 bytes
- `0x90` (chip ID) — return our synthesized chip ID
- `0xB7 + addr` (read data, KEY2) — once secure-area-decrypted, this is the bulk read path
- KEY1 commands `0x3C/0xB8/...` — only if loading an encrypted ROM with original secure area; for unencrypted/dumped ROMs we skip the KEY1 phase (most homebrew + Phase 1-7 testing)

### KEY1/KEY2 (Phase 9, only if needed)

KEY1 is Blowfish-style, key derived from the cart's gamecode + the BIOS-provided P-array. KEY2 is a stream cipher seeded by the cart's "card-ID-like" value. If we need to support encrypted ROMs (no$gba's `nogba.ini`-style game compatibility list), we'll port a known-good implementation from melonDS rather than reverse-engineer it ourselves.

### AUXSPI backup detection

Most NDS ROMs are listed in a public game-database table by `gamecode` → backup type. Until we have such a database, we offer:
- `--save-type {none,eeprom-512b,eeprom-8k,eeprom-64k,fram-32k,flash-256k,flash-512k,flash-1m}`
- A header byte at offset 0x14 (`device_capacity`) gives a hint we use as default

## Scheduler

Same min-heap pattern as GBA. Events:

| Event | Source |
|---|---|
| `HBlank` | every 1218 ARM7 cycles after frame start; sets HBlank flag, fires HBlank IRQ on both CPUs (if enabled), runs HBlank DMA9 (visible lines), emits scanline render call to both engines |
| `HBlankEnd` | start of next scanline |
| `VBlank` | line 192 transition; runs VBlank DMA on both CPUs |
| `Timer{cpu}{id}` | timer N's predicted overflow time |
| `DmaComplete{cpu, ch}` | when a long DMA finishes |
| `GxFifoLow` | when GXFIFO drops below half-full (ARM9 DMA mode 7) |
| `Slot1Done` | cart command transfer complete |
| `AuxSpiDone` | AUXSPI transfer complete |
| `AudioSample` | every ~1024 ARM7 cycles (32768 Hz) |
| `SwapBuffers` | 3D pipeline buffer swap at end of frame |

Cycles are ARM7-domain. We split each chunk between the two CPUs by running ARM9 for `2 × cycles` instructions for every `cycles` ARM7 cycles, refining later if accuracy demands.

### CPU scheduling — coarse plan

```
loop:
    let next_event_t = scheduler.peek().unwrap_or(target_t);
    let chunk_t = min(next_event_t, target_t);
    while scheduler.timestamp() < chunk_t:
        let arm7_cycles_target = chunk_t - scheduler.timestamp();
        // Run ARM9 for 2× arm7_cycles ARM9 cycles
        // Run ARM7 for arm7_cycles
        // Tick timers and audio
        // Drain pending cross-CPU effects (IRQ raises, GXFIFO triggers, IPC IRQs)
    while let Some(ev) = scheduler.pop_if_ready():
        handle(ev)
```

We start with **lockstep granularity = 1 ARM7 cycle** (so 2 ARM9 cycles) for correctness; we can switch to coarser interleaving once IPC paths are correct.

## Save State Strategy

Same as GBA: `#[derive(Serialize, Deserialize)]` on every state struct, top-level `Nds::save_state()` returns `bincode::serialize(&self)`. Compressed with zstd in the frontend.

The size budget is bigger than GBA:
- Main RAM 4 MB + WRAM 32 KB + ARM7 WRAM 64 KB + VRAM 656 KB + palette 2 KB + OAM 2 KB = ~4.7 MB raw
- After zstd: typically 200-500 KB

## BIOS HLE Strategy

We implement HLE for the SWIs both BIOSes expose, separately keyed:

**ARM9 BIOS (`bios/arm9.rs`)** — relevant SWIs:
| SWI | Name |
|---|---|
| 0x00 | SoftReset |
| 0x04 | IntrWait |
| 0x05 | VBlankIntrWait |
| 0x06 | Div |
| 0x07 | Reserved |
| 0x08 | Sqrt |
| 0x0B | CpuSet |
| 0x0C | CpuFastSet |
| 0x0D | GetCRC16 |
| 0x0E | IsDebugger |
| 0x0F | BitUnPack |
| 0x10-0x15 | LZ77 / Huffman / RL decompressors |
| 0x16-0x18 | DiffUnFilter (16-bit/8-bit/write) |

**ARM7 BIOS (`bios/arm7.rs`)** — superset with additional sound + WiFi + cart entries:
| SWI | Name |
|---|---|
| (most of the ARM9 set) | |
| 0x09 | GetSineTable / similar (returns from a 64-entry table) |
| 0x0A | GetPitchTable |
| 0x0B | GetVolumeTable |
| 0x19 | SoundBias |
| 0x1A-0x21 | SoundDriver* (mostly NOP for HLE; games that need them get a real BIOS) |

Like GBA, missing SWIs log a warning and return 0; load a real BIOS for full coverage.

## Crate Separation

```
nds-core (library)
├── Pure emulation logic
├── No platform deps (no SDL2, no filesystem)
├── All state serializable via serde
└── Usable by any frontend (SDL2, WASM, headless testing)

nds-frontend (binary)
├── SDL2 dual-screen window (256×192 stacked vertically with 90px gap, scaled)
├── BGR555 → RGB24 conversion
├── Keyboard → KEYINPUT (A=Z, B=X, X=C, Y=V, L=A, R=S, Start/Select=Enter/RShift, dpad=arrows)
├── Mouse on bottom screen → touch
├── Frame timing (~59.83 Hz)
├── Audio output via SDL2 callback + ring buffer
├── --bios-arm9 / --bios-arm7 / --firmware optional dumps
├── --save-type override
└── F5 / F8 save state hotkeys (matching GBA)
```

## Open Questions / Things We'll Resolve as We Build

- **3D rasterizer precision**: GBATEK is ambiguous on a few blend formulas. We'll cross-check melonDS output on our test scenes during Phase 7.
- **Slot-2 GBA passthrough**: out of scope; we return open bus on slot-2 reads. If we ever boot a "GBA Expansion Pack" game, this gets revisited.
- **Cache emulation**: deferred to Phase 9. Most games tolerate a write-through cache that's effectively transparent because they invalidate explicitly via CP15 c7 ops.
- **DSi-only features**: out of scope.

# Concept: The two CPUs

The NDS has two completely independent CPUs sharing a single 4 MB Main RAM. They run different code, at different clock rates, with different instruction sets, and exchange messages over [IPC](ipc.md). This is what most people mean when they say "the NDS is hard to emulate" — every other oddity flows from this.

## Identity

| | ARM7TDMI (carryover from GBA) | ARM946E-S (new) |
|---|---|---|
| Architecture | ARMv4T (1995) | ARMv5TE (1999) |
| Clock on NDS | 33.514 MHz | 67.028 MHz (exactly 2×) |
| Pipeline | 3-stage (fetch / decode / execute) | 5-stage (fetch / decode / execute / memory / writeback) |
| Caches | none | 8 KB I-cache + 4 KB D-cache (4-way set associative, 32 B lines) |
| TCM | none | 32 KB ITCM + 16 KB DTCM |
| MPU | none | 8-region MPU via CP15 c6 (we store but don't enforce) |
| System-control coprocessor | none | CP15 (cache + TCM + MPU + exception base) |
| Exception vector base | always `0x00000000` | software-selectable: `0x00000000` or `0xFFFF0000` (high vectors). NDS BIOS picks high. |

The ARM9's clock is exactly 2× the ARM7's, which is why our scheduler timestamps are in ARM7 cycles and we step the ARM9 twice per ARM7 step (the locked "lockstep at 1 ARM7 cycle" decision).

## TCM — Tightly-Coupled Memory

**Tightly-Coupled Memory** is a small, fast SRAM block wired *directly* into the CPU's load/store path, bypassing the system bus and the cache hierarchy entirely. Reads and writes complete in **a single cycle**, faster than even an L1 cache hit (there's no tag lookup, no way selection — it's just SRAM you can address).

The trade-off: you have to manage it explicitly. The compiler/linker / programmer decides what code or data lives there. Nothing migrates automatically the way cache lines do.

### ITCM vs DTCM

ARM946E-S (and therefore the NDS ARM9) has two separate TCM regions:

| | ITCM | DTCM |
|---|---|---|
| Purpose | hot code paths | hot data |
| Size | 32 KB | 16 KB |
| Base address | fixed at `0x00000000` (mirrored to fill the configured window) | software-relocatable; NDS BIOS puts it at `0x027C0000` |
| Configured via | CP15 `c9, c1, opcode2 = 1` | CP15 `c9, c1, opcode2 = 0` |
| Typical contents | IRQ handlers, sound mixer inner loops, geometry-pipeline command builder | IRQ stack, scratch buffers, math LUTs, frequently-touched globals |

Each TCM register encodes a `[base, size, enable]` triple. Writing it tells the CPU "from now on, addresses in `[base, base+size)` divert to this SRAM instead of going through the bus." The intercept is **physical** — the SRAM sits between the pipeline's load/store unit and the bus, so the bus never even sees the cycle.

In our emulator (`bus/arm9.rs`), `Bus9::read*` / `write*` check `itcm_region.contains(addr)` and `dtcm_region.contains(addr)` *first*, before any other decode. That's literally modelling "TCM intercepts before the bus."

### Why TCM exists at all

ARM9 was designed for embedded systems that needed deterministic timing — e.g. servo loops, IRQ handlers with hard deadlines. Cache is great for average throughput but terrible for worst-case latency: a cache miss can stall hundreds of cycles. TCM gives you a region where every access is provably one cycle, no surprises.

For the NDS specifically, the BIOS lays out the IRQ vector table at the bottom of ITCM (so vectoring through `0x18` is a single-cycle instruction fetch) and the IRQ stack at the bottom of DTCM (so push/pop in the handler doesn't go to Main RAM). The result: even though Main RAM access is many cycles, IRQ handlers run almost as fast as on a cacheless tightly-integrated SoC.

### TCM vs cache — they're not the same thing

Both are fast SRAM near the CPU. They are **fundamentally different things** doing different jobs:

| | Cache | TCM |
|---|---|---|
| What it *is* | A *copy* of data that lives in main memory | The actual *primary location* of the data — there is no "main memory copy" |
| Address space | Transparent — programs read main-RAM addresses; hardware silently intercepts based on tag match | Has its own dedicated address range (ITCM at `0x00000000`, DTCM wherever CP15 puts it) |
| Who decides what's in it | Hardware — automatically loads on miss, evicts under capacity pressure | Software — you explicitly copy code/data in at boot; nothing leaves until you tell it to |
| Tags / lookup | Each line has a tag; reads do tag compare + way selection | None. Just addressed SRAM. |
| Misses | Yes. Cold / capacity / conflict — all stall hundreds of cycles | None possible. The data is *there* by definition |
| Coherence | Hardware must track dirty lines, write back, sometimes invalidate | None. There's only one copy |
| Worst-case latency | Hundreds of cycles (miss + main RAM fetch) | One cycle. Always |
| Average throughput | Higher (giant working sets, automatic) | Limited to TCM size (~32 KB) |

The mental shift:

> **Cache asks "do I happen to have a copy of `0x02001000`?"**
> **TCM says "addresses `0x00000000`–`0x00007FFF` literally *are* this 32 KB SRAM block."**

If you put your IRQ handler in ITCM at boot, it sits there for the lifetime of the system. Nothing evicts it. No warm-up run is needed to bring it into the fast tier. Every fetch is provably one cycle, forever.

If you put the same handler in main RAM and rely on the I-cache, the *first* run pays a cold-miss penalty. After that the cache holds a copy — but if a 5 KB texture decompression runs between IRQs and trashes the cache lines, the *next* IRQ pays the miss penalty again. You don't notice on average. You notice when an audio handler misses its 32 µs deadline.

Concrete NDS BIOS / libnds boot sequence:

```
1. Copy ARM9 IRQ vector table   → ITCM at 0x00000000
2. Copy IRQ handler dispatch    → ITCM at 0x00000040
3. Allocate IRQ stack           → top of DTCM
4. Enable I/D caches for main-RAM regions via CP15 c1
5. Jump to game code (lives in main RAM, runs cached)
```

Steps 1-3 use TCM because the IRQ entry path can't tolerate cache misses. Steps 4-5 use cache because the game's hundreds-of-KB working set won't fit in TCM and missing on game logic is tolerable. They're **complementary, not competitors**:

- **Cache** = "make my huge program *average* fast, automatically."
- **TCM** = "make this small specific region *deterministically* fast, manually."

#### Why we model TCM but not cache

In our emulator, CP15 c7 cache-control ops (clean / invalidate / drain) are NOPs in `cpu/cp15.rs`. We don't simulate cache *contents*, so there's nothing to clean. Every access just goes through the bus. A working program runs identically with or without a modeled cache — just at different speeds, which we don't currently care about.

TCM, by contrast, is **not** transparent — it occupies its own address range. If we didn't model TCM, an ARM9 BIOS write to `0x00000000` would land in main RAM (or open bus) instead of in ITCM, and the IRQ vector table would be garbage. So `Bus9::read*` / `write*` check `itcm_region.contains(addr)` and `dtcm_region.contains(addr)` *first*, before any other decode — modelling exactly the "intercept before the bus" behavior of real hardware.

The takeaway: **cache is a performance optimization the program doesn't have to know about; TCM is part of the address space the program *does* have to know about.** That's why we can skip cache for now and revisit it in Phase 9 only if a game depends on its timing.

## ARMv5TE — what's new in the ARM9

The ARM946E-S understands every ARMv4T instruction the ARM7 does, *plus* the ARMv5TE additions. This is why our `Cpu` struct has an `is_arm9: bool` flag — the same decoder serves both, but the new encodings only fire on the ARM9.

### New instructions

| Mnemonic | What it does | Why it matters |
|---|---|---|
| `CLZ Rd, Rm` | Count leading zeros of `Rm` into `Rd` (single cycle). | Float → int normalize, priority encoders, log2 fast paths. |
| `BLX <imm>` | Branch with link, signed 24-bit offset, always switches to THUMB. | ARM-mode code calling THUMB-mode subroutines. |
| `BLX Rm` | Branch with link, target from register, mode from bit 0. | Indirect interworking calls (function pointers between ARM/THUMB). |
| `QADD Rd, Rm, Rn` | `Rd = SAT(Rm + Rn)`, sets `Q` bit on saturation. | Audio mixing without overflow wrap. |
| `QSUB`, `QDADD`, `QDSUB` | Saturating subtract; saturating-double-then-add/sub. | Audio + DSP arithmetic chains. |
| `SMLA<x><y> Rd, Rm, Rs, Rn` | `Rd = (Rm.<x> × Rs.<y>) + Rn`, where `<x>`, `<y>` ∈ {`B`, `T`} pick low/high 16-bit halves. Single cycle. | Audio FIR filters, 3D vector dot products. |
| `SMLAW<y>`, `SMULW<y>` | Multiply full 32-bit `Rm` by half-register `Rs.<y>`, take top 32 bits, optional accumulate. | Q15.16 fixed-point math. |
| `SMLAL<x><y>` | 64-bit signed accumulate of half-register multiply. | Sample summation across long buffers. |
| `SMUL<x><y>` | Half-register multiply, no accumulate, no saturation. | Pure DSP multiply. |
| `LDRD Rd, [...]` | 64-bit load into `(Rd, Rd+1)`. `Rd` must be even. | Atomic-ish 8-byte load, useful for matrix rows. |
| `STRD Rd, [...]` | 64-bit store from `(Rd, Rd+1)`. | Same. |
| `MCR`, `MRC`, `CDP` | Coprocessor data movement / processing. CP15 is the only coprocessor on NDS. | Cache control, TCM placement, MPU programming. |
| `MCRR`, `MRRC` | Two-register variants. | Rare on NDS. |
| `PLD [Rn, #imm]` | Cache preload hint. NOP-able. | We ignore it; no functional effect. |

THUMB also gains two encodings on ARMv5T:
- `BLX Rm` (the THUMB Format 5 op = `11`, `H1` = `1` form) — interworking call to a register target.
- `BLX <imm>` suffix at `0xE8..0xEF` — paired with a normal `BL` prefix at `0xF0..0xF7`, but the suffix in this range means "call ARM, not THUMB".

### Subtler semantic shifts

These aren't new instructions — they're behavioral changes to encodings that already existed on ARM7. Our decoder branches on `is_arm9` to pick the right one.

- **Writing PC auto-interworks.** Almost any ARMv5 instruction that ends up writing PC (`ADD PC, ...`, `MOV PC, ...`, `LDR PC, ...`, `LDM ..., {..., PC}`, `POP {..., PC}`) looks at bit 0 of the value: 1 → switch to THUMB, 0 → ARM. ARMv4T just masks bit 0 off and stays in the current state. So `POP {..., PC}` returning to a THUMB caller from an ARM function "just works" on ARM9 but not on ARM7.
- **Misaligned `LDR` / `LDRH`.** ARM7 has the rotated-result quirk: a misaligned word load returns the aligned word rotated right by `(addr & 3) * 8`; a misaligned `LDRH` rotates right by 8. ARM9 force-aligns instead — no rotation, just clears the low bits of the address.
- **`Q` bit in CPSR.** ARM9 has a sticky saturation flag at bit 27, set by `Q*` and `SMLA*` ops. ARM7's CPSR bit 27 is reserved.

## CP15 — the system control coprocessor

CP15 is how the ARM9 talks to its caches, TCM, MPU, and exception-base configuration. It's accessed only via `MCR` (write) and `MRC` (read) — there are no memory-mapped CP15 registers.

The registers we model in `cpu/cp15.rs`:

| CRn | Name | What we do |
|---|---|---|
| c0 | ID + cache type | Read returns hardcoded ARM946E-S IDs |
| c1 | Control register | Bit 13 latches the high-vector base (`0xFFFF0000`); other bits stored verbatim |
| c2 | Cacheable region bits | Stored, not enforced |
| c3 | Write-buffer region bits | Stored, not enforced |
| c5 | Access permissions | Stored, MPU stub |
| c6 | 8 MPU regions | Stored; full enforcement deferred to Phase 9 |
| c7 | Cache maintenance ops (clean / invalidate / drain) | NOPs (we don't simulate cache contents) |
| c9 c1 op2=0 | DTCM region | Drives `dtcm_base` / `dtcm_size` in `Bus9` |
| c9 c1 op2=1 | ITCM region | Drives `itcm_size` in `Bus9` |

The TCM-region writes are the load-bearing ones — without them the ARM9 BIOS can't set up its IRQ vectors and the boot path fails. Cache control is intentionally a no-op because we don't model cache contents (a write-through cache that's effectively transparent).

## How the two are used on the NDS

The hardware is symmetric in many ways but asymmetric in the *I/O paths* it exposes to each CPU. Roughly:

| Resource | ARM9 sees | ARM7 sees |
|---|---|---|
| Main RAM (4 MB) | yes | yes |
| Shared WRAM (32 KB) | per `WRAMCNT` | per `WRAMCNT` |
| ARM7 WRAM (64 KB) | no | yes |
| 2D Engine A regs | yes | no |
| 2D Engine B regs | yes | no |
| 3D engine + GXFIFO | yes | no |
| VRAM banks A-I | yes (full) | only banks C/D when routed |
| Slot-1 cart bus | yes (default; switchable via `EXMEMCNT`) | switchable |
| AUXSPI (cart backup) | no | yes |
| Sound mixer (16 channels) | no | yes |
| SPI bus (firmware / touchscreen / PMIC) | no | yes |
| RTC | no | yes |
| WiFi | no | yes (out of scope for us) |
| Keypad (10 buttons) | yes | yes |
| EXTKEYIN (X/Y/lid/pen-down) | no | yes |

So in practice:
- **ARM9 is "the CPU running the game"** — game logic, 2D rendering, 3D rendering, cart loading.
- **ARM7 is a sound + I/O coprocessor** — its job is to service the ARM9's requests for "play this sample", "give me touch coords", "read this save block", "what time is it".

ARM9 sends commands over the IPC FIFO; ARM7 wakes on the recv-not-empty IRQ, services the command, sends results back. ARM9's IRQ handler picks them up. Most commercial games never directly read or write the sound or SPI registers from the ARM9 — they always go through the ARM7 indirection.

### Why two CPUs at all?

Two reasons:

1. **Cost / leverage.** The ARM7TDMI was already designed, debugged, licensed, and present in millions of GBAs. Re-using it for the I/O subsystem was free engineering. Nintendo paired it with one new high-performance core (ARM9) instead of designing a single integrated chip.
2. **Backwards compatibility.** Slot-2 GBA cartridge mode powers down the ARM9 entirely and lets the ARM7 run the GBA cart at GBA-clock speeds against the original GBA memory map. The NDS is literally a GBA when you plug a GBA cart in — same CPU, same I/O, just inside an NDS shell.

## What this means for our emulator

- A single `Cpu` struct with `is_arm9: bool` flag — locked architecture decision from Phase 1 setup.
- ARMv5TE encodings dispatched only when `is_arm9` is true (in `cpu/arm.rs` and `cpu/thumb.rs`); ARM7 falls through to `arm_undefined`.
- `cpu/cp15.rs` only meaningful when `is_arm9` is true; `MCR`/`MRC` to CP15 are NOPs on ARM7 (would also raise `arm_undefined`, but ARM7 BIOS code never tries).
- `Bus9` checks ITCM/DTCM regions first; `Bus7` doesn't have that path.
- Different exception bases threaded through `Cpu::handle_interrupt` and `software_interrupt` via `self.exception_base`, refreshed from CP15 c1 bit 13 after every CP15 write.
- Different interwork-on-PC-write behavior in `Cpu::set_reg_with_flags`.
- Different misaligned-access semantics in the LDR / LDRH executors.

All of that lives in `nds-core/src/cpu/`. The `is_arm9` flag is the load-bearing distinction — change it and the same struct goes from "GBA-grade ARM7TDMI" to "NDS-grade ARM946E-S".

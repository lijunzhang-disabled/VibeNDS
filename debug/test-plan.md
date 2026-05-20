# Phase 9 test plan

Ordered list of test ROMs to run, what each one validates, and what failure modes to expect. Work top to bottom: each stage assumes the previous stage is passing, so failures partway down are easier to attribute when the layers below are known-clean.

Each test becomes the source of one or more `debug/YYYY-MM-DD_<slug>.md` investigation logs. Delete entries from `debug/phase9_carryover.md` as the underlying gaps are closed.

---

## Stage 1 — CPU correctness floor

### 1.1 `armwrestler-ds`

**Source**: https://github.com/destoer/armwrestler-fix (most up-to-date fork; original Stephen Stair version exists too but the fork includes fixes).

**Build**:
```sh
git clone https://github.com/destoer/armwrestler-fix.git
cd armwrestler-fix && make
# produces armwrestler.nds
```

**Run**:
```sh
cargo run --release -p nds-frontend -- --rom path/to/armwrestler.nds
```

**What pass looks like**: top screen shows "All tests passed" for both the ARM7 and ARM9 modes. Use the keypad to switch between cores (per the in-ROM menu).

**Sub-tests it exercises** (in roughly this order on screen):
- ARM data processing (32 tests)
- Multiplication (10 tests)
- Multiply long (10 tests)
- Single data transfer (10 tests)
- Halfword + signed transfer (10 tests)
- Block data transfer (10 tests)
- PSR transfer (MRS/MSR) (10 tests)
- THUMB equivalents of the above
- ARMv5TE-specific: CLZ, BLX, Q* saturating arith, SMLA* DSP multiply, LDRD/STRD (ARM9 mode only)

**Failure modes & what they mean**:

| Symptom | Likely cause |
|---|---|
| White screen / CPU escapes | Pipeline ordering / decoder dispatch bug. Run with `INSTR_TRACE_RING=1` if we've added that diagnostic. |
| "Test N failed" with N in 1..40 | Specific ARM instruction decoder bug. Look up which test in the ROM source. |
| Tests pass on ARM7 but fail on ARM9 | ARMv5TE-only encoding (`CLZ`, `BLX`, `Q*`, `SMLA*`, `LDRD/STRD`, MCR/MRC) or interwork/misaligned-access difference. |
| Tests fail on ARM7 but pass on ARM9 | Misaligned-access quirk (ARM7 has rotated-result; ARM9 force-aligns). |

**Done when**: both ARM7 and ARM9 modes show "All passed."

### 1.2 jsmolka GBA `arm.gba` / `thumb.gba`

**Why also these**: armwrestler-ds doesn't cover every encoding edge case. The GBA project caught 5 CPU bugs via these ROMs that armwrestler missed (see `../gba/debug/2026-04-24_cpu-accuracy-sweep.md`). Since we share the ARM7TDMI core, these still apply — load them in slot-1 mode (CPU runs fine without 3D/audio).

**Source**: https://github.com/jsmolka/gba-tests

**Run**: same as above with the `.gba` file. Frontend treats it as a slot-2 GBA cart; relevant tests still execute.

**Done when**: outputs match the GBA project's known-good results.

---

## Stage 2 — Memory / timing floor

### 2.1 `rockwrestler`

**Source**: https://github.com/PSI-Rockin/dual_orange — small NDS test ROMs for memory access widths, timer/IRQ edges, DMA timing.

**What it exercises**:
- 8/16/32-bit read+write to every NDS memory region (Main RAM, ARM7 WRAM, shared WRAM with WRAMCNT modes, VRAM at every bank config, palette, OAM)
- Timer overflow + cascade
- DMA fire timing
- IRQ delivery (IPC sync, IPC FIFO, timer, V/HBlank)

**Failure modes**:

| Symptom | Likely cause |
|---|---|
| Wrong width reads (e.g. 16-bit read of palette returns the byte broadcast) | Bus width handling on a specific region. We don't yet implement 8-bit-bus broadcast semantics for VRAM/palette — most likely culprit. |
| Timer cascade doesn't fire | Cascade detection in `timer.rs`. |
| IPC FIFO IRQ misses an edge | Empty→non-empty transition detection in `ipc.rs`. |
| DMA fires the wrong number of words | Off-by-one in word-count latching at enable-rising-edge. |

**Done when**: all rockwrestler sub-tests pass (it shows pass/fail counts on screen).

---

## Stage 3 — 3D engine proof

### 3.1 devkitPro 3D examples

**Source**: install devkitPro (`pacman -S nds-examples`) or grab from https://github.com/devkitPro/nds-examples/tree/master/Graphics/3D.

**Recommended order**:

1. **`Simple/Textured_Cube`** — single textured quad-cube spinning. Tests: matrix stacks (PUSH/POP per face), one texture format (probably 16-color or 256-color), basic depth test.
2. **`Lighting/Specular`** — multi-light scene with Phong specular. Tests: lighting LUT, materials, per-vertex normals.
3. **`Effects/Cel_Shading`** — toon shading via the TOON_TABLE. Tests: `POLYGON_ATTR.mode=2` red-channel remap (currently stubbed — this *will* fail until 3D-3 in the carry-over lands).

**What pass looks like**: visible rendering matching the reference (compare against melonDS).

**Failure modes**:

| Symptom | Likely cause |
|---|---|
| Black screen | DISPCNT BG0=3D bit, DISP3DCNT enable bit, or SWAP_BUFFERS not firing. Check via `cargo run` with a print at `swap_buffers`. |
| Cube renders but textures missing | Texture format we don't handle yet — likely format 5 (block-compressed). 3D-1 carry-over. |
| Cube renders but is the wrong color everywhere | Vertex color or lighting bug. Compare polygon attr decoding. |
| Cube renders with z-fighting / depth flicker | Z vs W buffer mode mismatch (3D-5 carry-over) or depth-test direction wrong. |
| Polygons appear flipped inside-out | Winding order in strips (TriangleStrip's odd-N winding flip) or cullface bit handling. |
| Polygons missing on one side | Near-plane clipping rejecting too aggressively, or backface culling backwards. |

**Done when**: at least one textured-cube demo renders correctly.

---

## Stage 4 — First commercial title

### 4.1 *Phoenix Wright: Ace Attorney*

**Why this one first**:
- 2D-only (no 3D pipeline stress; isolates IPC + BIOS HLE + AUXSPI bugs).
- Heavy IPC traffic (touch input flows ARM9→ARM7→ARM9 every frame).
- AUXSPI EEPROM save (we wired this in Phase 5).
- Boots to first interactive screen within ~3 seconds — fast iteration loop.

**Suggested test sequence**:
1. Boot to Capcom logo. Pass if logos animate, fail if black/white screen.
2. Boot to title screen. Pass if music plays and "Start Game" prompt visible.
3. Start a new game. Pass if first courtroom scene renders, text appears.
4. Advance 10 lines of dialogue with touch + B. Pass if input is responsive.
5. Save game. Quit. Reload `.nds`. Resume. Pass if the save survived.

**Expected first failure** (extrapolating from typical emulator bring-up):
- The "advance dialogue" step almost always hits some BIOS SWI we haven't implemented (probably `SoundDriver*` or one of the audio HLE SWIs). Solution: load a real ARM7 BIOS dump via `--bios-arm7` and re-test.

**Failure modes**:

| Symptom | Likely cause |
|---|---|
| Black screen at boot | Direct-boot path / cart header parse / IPC handshake. Inspect with print at `Nds::load_cart_direct_boot`. |
| Boots to white screen | Engine A DISPCNT not set; either bit 3 = 1 but 3D not enabled, or display mode wrong. |
| Music garbled / wrong tempo | Sound channel timing — `tmr` period interpretation or mixer cycles-per-sample. |
| Saves don't survive | AUXSPI WEL latch or backup-type mismatch (try `--save-type eeprom-64k`). |

**Done when**: at least the boot → title → first scene path works. Save-load is a stretch goal.

### 4.2 Subsequent commercial titles (rough priority)

After Phoenix Wright is at least booting:

| Title | Why next | Stresses |
|---|---|---|
| *New Super Mario Bros.* | 3D BG0 used heavily, but simple geometry | 3D rasterizer path through Engine A BG0 |
| *Mario Kart DS* | 3D + audio + slot-1 streaming | Full system stress, except WiFi |
| *Pokémon HeartGold* | RTC, AUXSPI flash saves (not EEPROM), complex IPC | RTC, larger backup, save flakiness |
| *Trauma Center: Under the Knife* | Heavy 3D with edge marking + toon | Post-effects (currently partially stubbed) |

Each new title typically surfaces 1–3 new bugs. Track each as a dated debug log.

---

## How to use this plan

- Work **top to bottom**. Don't start commercial titles until rockwrestler and a devkitPro 3D demo work — failures past that point are otherwise impossible to attribute.
- Each test failure → a dated `debug/YYYY-MM-DD_<slug>.md` log following the template in `debug/README.md` (symptom → investigation → root cause → fix → regression test → verification).
- When a Phase 9 carry-over item turns out to be the root cause, delete it from `debug/phase9_carryover.md`.
- After every fix, re-run the prior stages to make sure nothing regressed. The unit-test suite (currently 261) catches most regressions; the test-ROM suite catches the ones that need a real ROM context to surface.

## What "Phase 9 done" looks like

A reasonable bar for Phase 9 closure:

- **Stage 1**: armwrestler-ds 100% pass on both ARM7 and ARM9 modes.
- **Stage 2**: rockwrestler 100% pass.
- **Stage 3**: ≥ 2 devkitPro 3D examples render correctly with screenshot diff against melonDS.
- **Stage 4**: ≥ 5 commercial titles boot to playable state; at least 3 of them complete a save round-trip.

That's the original plan-level milestone ("5 titles boot to title screen; stretch goal 20+ playable to first save") made concrete.

Beyond that, Phase 9 is open-ended — every additional commercial title is more polish. There's no natural "done" state; you stop when the bug curve flattens or you switch projects.

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
- After every fix, re-run the prior stages to make sure nothing regressed. The unit-test suite (currently 323) catches most regressions; the test-ROM suite catches the ones that need a real ROM context to surface.

## Current campaign notes (2026-05-31)

User-requested sweep order:

1. `armwrestler-fixed` — **done**. All pages pass via direct boot. See
   `debug/2026-05-31_armwrestler-fixed-direct-boot.md`.
2. `arm7wrestler` — **all seven menu pages pass** on the locally patched
   build at `/private/tmp/arm7wrestler/source.nds`. The upstream source needs
   temporary build compatibility patches with modern devkitPro:
   `arm7/Makefile` must allow ARMv5TE opcodes so the ARM7 undefined/no-op
   tests can assemble; obsolete ARM7 library links must be removed; minimal
   modern crt0 symbols must be supplied; ARM9 helper was patched to copy the
   ARM7 draw buffer from `0x02300000` and poll VBlank instead of relying on
   `SWI 0x05`.
   - Page sweep evidence: `ARM ALU`, `ARM LDR/STR`, `ARM LDM/STM`,
     `THUMB ALU`, `THUMB LDR/STR`, `THUMB LDM/STM`, and `ARM V5TE` each
     rendered visible `OK` rows with nonzero bottom framebuffer.
   - Failures fixed during the sweep:
     - ARM7 `LDM rN!, {...rN...}` now suppresses writeback whenever the base
       register is in the load list. ARM9 keeps its previous lowest-register
       writeback behavior.
     - Direct-boot ARM7 now initializes an Undefined-mode stack.
     - The synthetic no-BIOS ARM7 undefined vector now jumps through the
       handler slot at `0x0380FFDC`, matching the handler installed by
       `arm7wrestler`.
     - When that handler slot is absent, the synthetic no-BIOS ARM7
       undefined vector returns immediately. This preserves modern libnds
       direct-boot startup, which does not install the old arm7wrestler
       undefined-handler slot.
     - ARM7 CP14 `MRC` acts as a harmless no-op while CP15 still raises
       undefined, matching the test's ARM7 expectation.
     - ARM7 DSP multiply encodings (`SMLA*` family as tested) act as no-ops
       instead of raising undefined.
3. devkitPro `hello_world`, `input/Touch_Pad`, `time/*`, `pxi` — **basic
   startup smoke now passes for the built set**. Initial failure mode was a
   white frame with no VRAM writes and ARM9 stuck in the bootstub IPC/FIFO
   startup path. Current fixes/progress:
   - ARM9/ARM7 `SWI 0x0F` HLE now reports the normal DS boot path.
   - ARM7 `SWI 0x0E` HLE reports no debugger.
   - No-BIOS ARM7 direct boot now supplies a synthetic IRQ vector that jumps
     through the calico/libnds handler slot at `0x0380FFFC`.
   - ARM9 CP15 wait-for-interrupt (`MCR p15,0,Rd,c7,c0,4`) now parks the CPU
     instead of running into adjacent ITCM exception stubs.
   - ARM7 `HALTCNT` writes at `0x04000301` now request CPU halt through the
     top-level run loop.
   - IPC FIFO send-empty IRQ is now raised when the send FIFO drains to empty
     and when the IRQ is enabled while the send FIFO is already empty.
   - ARM7 `0x0381xxxx`/`0x0382xxxx`/`0x0383xxxx` accesses no longer alias the
     canonical private WRAM window at `0x03800000..0x0380FFFF`; in WRAMCNT
     mode 3 those mirrors route to shared WRAM. This prevents modern libnds
     startup code from clearing ARM7 runtime code copied to private WRAM.
   - In direct-boot mode, unhandled ARM9/ARM7 SWIs now return instead of
     jumping into absent BIOS vectors. This avoids the ARM7 looping through
     synthetic IRQ-vector code after hitting libnds BIOS-call veneers.
   - ARM9 no-BIOS ITCM reads now recognize the compact calico vector layout
     used by modern libnds and redirect IRQ vector fetches to calico's IRQ
     handler instead of the reset/prefetch-abort stubs.
   - ARM `LDM...^` exception returns now perform writeback before restoring
     CPSR on a load-to-PC, so `ldmia sp!, {...,pc}^` updates IRQ-mode `SP`
     instead of corrupting the restored mode's `SP`.
   - ARM7 synthetic IRQ vector now preserves interrupted `R0-R3`, `R12`, and
     `LR` even when the libnds handler slot is still null. This fixed the
     firmware-read loop corruption where `R0=0x1ff` was replaced by the IRQ
     handler-slot address.
   - Direct-boot BIOS HLE now covers the libnds startup calls seen so far:
     `SWI 0x02`/`0x06` halt-style waits, `SWI 0x03` delay/WaitByLoop,
     `SWI 0x07` ARM7 sleep, `SWI 0x09` divide, and `SWI 0x0D` sqrt.
     `SWI 0x03` is a delay loop used by ARM7 SPI/PXI startup code; treating
     it as Halt parked ARM7 before it could wake ARM9.
   - ARM9 CP15 c13,c0,1 now round-trips. Calico's IRQ trampoline uses it as
     a scratch IRQ-mask register before waking scheduler waiters.
   - ARM exception returns (`MOVS PC,LR` and `LDM...PC^`) now preserve the
     restored SPSR Thumb bit instead of re-interworking from the even return
     address. This fixed a `pxi` crash where Thumb console code was resumed
     as ARM.
   - Direct boot now clears the ARM9 high-vector bit and, in synthetic-BIOS
     mode, high-vector fetches use an installed low-ITCM vector table before
     falling back to the no-BIOS IRQ wrapper. This helps old homebrew that
     toggles CP15 high vectors without a real BIOS image available.
   Smoke results from the temporary probe:
   - `hello_world` reaches `main()`, `consoleDemoInit`, and display setup.
   - `pxi` reaches `main()`/`consoleDemoInit` and no longer falls into the
     calico exception loop.
   - `time/RealTimeClock`, `time/timercallback`, and `time/stopwatch` reach
     main/display with no exception stack.
   - `input/Touch_Pad/touch_look` and `input/Touch_Pad/touch_test` reach
     display setup, produce nonzero pixels, and do not hit the suspected
     low-vector/ITCM exception paths.
   Fresh regression smoke after the arm7wrestler fixes:
   - `hello_world` at 600 frames: top nonzero `1933`, bottom nonzero `49152`,
     `DISPCNT_B=0x00010100`, ARM9/ARM7 both halted in the expected calico
     wait path.
   - `pxi` at 600 frames: top nonzero `1121`, bottom nonzero `49152`,
     `DISPCNT_B=0x00010100`, ARM9/ARM7 both halted in the expected calico
     wait path.
4. Representative devkitPro Graphics examples — **startup/display smoke now
   passes for the built set, with initial bitmap/3D rendering evidence**:
   `Backgrounds/16bit_color_bmp`, `Backgrounds/256_color_bmp`,
   `Backgrounds/rotation`, `Sprites/simple`, `3D/Simple_Tri`,
   `3D/Simple_Quad`.
   - Fixed during this pass:
     - ARM9 hardware divide/square-root registers at `0x04000280..0x040002BF`
       are now implemented. This lets libnds fixed-point helpers build a
       real `gluPerspective` matrix instead of sending zero scale terms to
       the 3D engine.
     - GPU3D matrices now use the NDS row-vector/row-major command
       convention, with clip composition as position then projection.
     - `DISP3DCNT` bit 0 is no longer treated as a global 3D-enable bit; it
       is a texture-mapping feature bit. `Simple_Tri` sets anti-aliasing
       (`0x0010`) and should still rasterize.
     - Mode 3-5 extended affine bitmap BG rendering now covers BG2/BG3
       256-color and direct-color bitmap forms, including mode 5 BG3 cases
       used by the devkitPro bitmap samples.
   - Fresh 600-frame probes:
     - `3D/Simple_Tri`: `fb3d_nonzero=9350`, displayed framebuffer
       nonzero `9350`, `DISPCNT_A=0x00010108`, `DISP3DCNT=0x0010`.
     - `3D/Simple_Quad`: `fb3d_nonzero=18768`, displayed framebuffer
       nonzero `18768`, `DISPCNT_A=0x00010108`, `DISP3DCNT=0x0010`.
     - `Backgrounds/16bit_color_bmp`: displayed framebuffer nonzero
       `45364`, unique colors `3285`, `BG3CNT_A=0x4084`.
     - `Backgrounds/256_color_bmp`: displayed framebuffer nonzero `48419`,
       unique colors `256`, `BG3CNT_A=0x4080`.
     - `Backgrounds/rotation`: displayed framebuffer nonzero `47601`,
       unique colors `234`, `BG3CNT_A=0x4080`.
   The smoke checks still do not prove pixel-perfect rendering, texture
   correctness, every BG size, or post-effect accuracy.
5. devkitPro audio and filesystem/card groups — **representative behavior
   now passes where the emulator has a backing device**:
   - `audio/maxmod/basic_sound` responds to injected `KEY_A`/`KEY_B`, starts
     multiple sound channels, and produces nonzero mixed PCM output through
     frame 239. `KEY_A` probe: `master=8064`, `active=5`,
     `max_active=7`, `audio_written=263102`, `audio_nonzero=197788`.
     `KEY_B` probe: `active=4`, `max_active=6`, `audio_nonzero=187388`.
   - `audio/maxmod/audio_modes` also produces sustained output:
     `master=8064`, `active=8`, `max_active=8`,
     `audio_written=263102`, `audio_nonzero=238366`.
   - `filesystem/nitrofs/nitrodir` now mounts NitroFS through direct Slot-1
     card reads and lists the embedded directory tree/files. A 600-frame
     console probe shows `nitro://file1.txt`, `nitro://dir1`,
     `nitro://dir1/test.txt`, `nitro://dir2/subdir1/file2.txt`, etc.,
     instead of the previous `nitroFSInit failure: terminating`.
   - The NitroFS fix required two emulator-side pieces:
     - Direct boot now clears the modern Calico argv header overlay at
       `0x02FFFE70` and points `argv` at a null slot before ARM9 can race
       ARM7 startup. Otherwise ARM9 can read stale NDS header bytes as a
       non-null `argv[0]` and skip direct card access.
     - `EXMEMCNT` at `0x04000204` now defaults Slot-1 ownership to ARM7
       and is writable by ARM9. Calico's `ntrcardOpen()` uses bit 11 to
       decide whether it must initialize Slot-1 main-mode card reads.
   - Minimal Slot-1 ROM command support now covers command registers,
     `ROMCTRL`, `CARD_DATA_RD`, header reads, chip-ID reads, and `0xB7`
     main-data reads from the loaded ROM image. Unit coverage exercises
     command byte order, data read offsets, status clearing, and EXMEMCNT
     ownership.
   - `card/eeprom` now opens Slot-1, reads the cart header, detects the
     configured EEPROM through old-libnds AUXSPI probes, and reads backup
     bytes. A 600-frame probe reports `Reading cart info...`,
     `Game ID: HOMEBREW`, `Type: 2`, and `Size: 8192`. With an injected
     `KEY_A` press it advances to `First 160 bytes of EEPROM` and dumps
     erased `ff` bytes from the emulated backup storage.
   - The EEPROM behavior fix required AUXSPI transaction reset on chip
     deselect via `AUXSPICNT` writes. Old libnds ends commands by writing
     `AUXSPICNT = 0x40`; without treating that as deselect, the next command
     was interpreted as a dummy byte for the previous command. EEPROM
     `RDID` now returns `0xFFFFFF` while flash chips keep the placeholder
     JEDEC ID path, matching libnds' type probe expectations.
   - `filesystem/libfat/libfatdir` still needs a ROM-level pass. The emulator
     now has an initial DLDI/PXI block-device service that answers Calico
     block-device channel requests for DLDI init, presence, sector reads, and
     sector writes against a synthetic FAT16 image. Unit coverage verifies
     sector-count exposure and boot-sector reads. The actual devkitPro
     `libfatdir` ROM has not yet produced a successful root directory
     listing, so keep this stage open.
   - Current core regression count after these fixes: `cargo test -p
     nds-core` reports `326 passed; 0 failed`.
6. Homebrew games/demos — **first broad candidates tested**:
   - Shared startup fix: direct-boot/no-BIOS ARM9 IRQ delivery now
     acknowledges enabled pending IRQs when the libnds handler slot at
     `0x02FF3FFC` is still null. Several older homebrew ROMs enable VBlank
     before installing a handler; without this synthetic-BIOS behavior they
     repeatedly re-entered the high-vector IRQ wrapper during startup.
     The same path now also sets the old-libnds DTCM IRQ shadow word before
     acknowledging the hardware IF bit. This lets old ARM-side
     `swiWaitForVBlank` loops observe VBlank even when no BIOS/libnds IRQ
     trampoline has been installed yet.
   - Engine B BG/OBJ extended palettes are now rendered. Flappy uses
     `VRAM_H` as Engine B BG extended palette and `VRAM_I` as Engine B OBJ
     extended palette, with normal BG/OBJ palette memory left zero; without
     these paths the game was black or had black sprite silhouettes.
   - hbmenu `argvTest` now renders the expected `No arguments!` text and both
     CPUs halt cleanly by 600 frames: top nonzero `263`, bottom nonzero
     `49152`, `DISPCNT_B=0x00010100`, `pc9=0x02001fee`,
     `pc7=0x03802300`.
   - `cellsDS` now reaches visible startup text instead of the previous
     flat-frame/reset-like state. The current capture shows title text plus
     `Unable to open the directory. Please make sure that /cellsds/snapshots
     exists` and `loading default engines...`; top nonzero `1506`, bottom
     nonzero `3000`, `DISPCNT_A/B=0xc0211953`. This is a useful startup pass,
     but not an end-to-end app pass because it needs expected files/directories
     on a filesystem path the emulator does not yet provide.
   - `neo-engine/neo` now reaches a proper title/splash screen instead of the
     previous barcode-like/text-fragment output. A 600-frame capture shows the
     Pokemon Neo splash with version/date text; top nonzero `48371`, bottom
     nonzero `48194`, `DISPCNT_A/B=0x00011d15`, both CPUs halted in runtime
     wait paths.
   - `Fewnity/Flappy-Bird-Nintendo-DS` now reaches a visible title/get-ready
     screen with colored BG and sprites. A 600-frame capture reports top
     nonzero `49152`, top unique colors `81`, `DISPCNT_A=0x40010000`,
     `DISPCNT_B=0xc0111c10`, BG2/BG3 active on Engine B, and both CPUs
     halted in VBlank wait paths.
   - `Spelunky DS` now reaches its title/menu screen. A 600-frame capture
     shows the cave/title art plus menu text; top nonzero `49152`, bottom
     nonzero `49152`, `DISPCNT_A=0x00111910`,
     `DISPCNT_B=0x00111810`, and both CPUs halted in VBlank wait paths.
   - `cellsDS.sc.nds` is not a direct-boot candidate in its current form:
     the header load values point outside the ROM image and the emulator
     rejects it with `OutOfRangeRom`.

Tooling used:

- `devkitpro/devkitarm:20260221` Docker image for builds.
- `/private/tmp/arm7wrestler` and `/private/tmp/nds-examples` as source
  checkouts.
- `/private/tmp/nds-homebrew-flappy` and `/private/tmp/nds-homebrew-neo` as
  first homebrew-game checkouts.
- Built ROMs copied or run from ignored/temp paths; `test-roms/` remains
  ignored.

EmuDev resource check:

- `https://github.com/emudev-org/discord-resources` mirrors the EmuDev
  Discord system resources. Its Nintendo DS section confirms the current
  queue: GBATEK, Shonumi's NDS docs, `mic-/armwrestler`,
  `Arisotura/arm7wrestler`, devkitPro `nds-examples`, and melonDS research.
- The same section links targeted hardware references for RTC, touchscreen,
  and firmware flash. Use those when the `time/*`, `input/Touch_Pad`, and
  firmware/SPI paths move from "boots" to behavioral validation.
- Its ARM section links the ARM7TDMI, ARM9E-S, ARM946E-S, and ARMv5TE
  manuals. These are the right references when CPU fixes disagree with
  armwrestler/arm7wrestler results.
- The listed Discord CDN built-ROM links for armwrestler/arm7wrestler are not
  reliable long-term. Prefer source builds or locally archived ROMs under
  ignored `test-roms/`.
- It also lists `https://tcrf.net/Aging_Card_NTR` as a possible diagnostic
  target. Treat this as a later reference/hardware-test path only; it may
  require legally sourced Nintendo diagnostic media rather than open homebrew.
- `https://github.com/asiekierka/awesome-dsdev` is a useful index for
  additional open homebrew, demos, tools, and libraries. It is less of a
  conformance-test suite, but good for broadening the post-devkitPro
  compatibility set with legal ROMs we can build from source.
- `https://github.com/devkitPro/nds-examples` remains the best structured
  homebrew regression source because its directories cover graphics, audio,
  card/filesystem, input, PXI, and time in small focused programs.

## What "Phase 9 done" looks like

A reasonable bar for Phase 9 closure:

- **Stage 1**: armwrestler-ds 100% pass on both ARM7 and ARM9 modes.
- **Stage 2**: rockwrestler 100% pass.
- **Stage 3**: ≥ 2 devkitPro 3D examples render correctly with screenshot diff against melonDS.
- **Stage 4**: ≥ 5 commercial titles boot to playable state; at least 3 of them complete a save round-trip.

That's the original plan-level milestone ("5 titles boot to title screen; stretch goal 20+ playable to first save") made concrete.

Beyond that, Phase 9 is open-ended — every additional commercial title is more polish. There's no natural "done" state; you stop when the bug curve flattens or you switch projects.

# Armwrestler fixed — direct boot and ARM core fixes

Date: 2026-05-31
Status: **Fixed**

## Symptom

Running `armwrestler.nds` from `Atem2069/armwrestler-fixed` via direct
boot initially failed, then showed a black screen:

```sh
cargo run --release -p nds-frontend -- --rom test-roms/armwrestler.nds --no-audio --scale 2
```

After the display path was fixed, Armwrestler rendered test pages but
reported failures in `LOAD TESTS PART 1` and `LDM/STM TESTS 1`.

## Investigation

The ROM header showed an ARM7 binary loaded at `0x03800000`, so direct
boot needed to support ARM7 private WRAM, not only Main RAM.

Once direct boot reached ARM9 code, a probe showed the ROM entering
`DrawText` but exiting immediately. The copied string data in Main RAM was
correct, yet `MOV r3, r3, LSL #1` corrupted `r0`, the string pointer. That
isolated the issue to ARM instruction decoding rather than VRAM or text
data.

After the text renderer worked, the ROM wrote pixels to LCDC VRAM at
`0x06800000`, but the frontend still displayed black. The GPU path did not
yet implement Engine A display mode 2, the direct-VRAM framebuffer mode
used by this ROM.

The final two failing screenshots showed:

- `LOAD TESTS PART 1`: plain `LDR` forms returning bad `Rd` values.
- `LDM/STM TESTS 1`: `LDM` forms returning bad `Rd` / `Rn` values while
  `STM` forms passed.

Reading the Armwrestler source narrowed those to unaligned word `LDR`
rotation semantics and `LDM ...!` writeback when the base register is in
the load list.

## Root Causes

1. **Direct boot only copied binaries into Main RAM.**
   Some homebrew, including this ROM, places the ARM7 stub in private ARM7
   WRAM at `0x03800000`.

2. **ARM9 startup TCM state was too empty for homebrew crt0 code.**
   Armwrestler expands ITCM to a low-address mirror and uses an ITCM stack
   around `0x00803EBC`; direct boot needed an initial ITCM region, and CP15
   needed to accept the ROM's even ITCM control value.

3. **ARMv5 DSP multiply decode was too broad.**
   It matched ordinary shifted-register data-processing opcodes such as
   `MOV r3, r3, LSL #1`, corrupting registers in `DrawText`.

4. **Engine A direct-VRAM display mode was unimplemented.**
   Armwrestler maps VRAM bank A as LCDC and displays it using display mode
   2.

5. **ARM9 unaligned word `LDR` did not rotate.**
   Armwrestler expects the ARM architectural behavior used by ARM7TDMI-style
   word loads: read the aligned word and rotate right by `8 * (addr & 3)`.

6. **`LDM` writeback skipped the base-is-lowest-register case.**
   For `LDMIB r3!, {r3,r5}`, writeback to `r3` must be visible after the
   load. When the base register appears later in the list, the loaded value
   wins.

## Fix

- `cart::direct_boot::apply` now receives `Arm7Memory` and can copy ARM7
  binaries into private WRAM.
- Direct boot initializes ARM9 ITCM, and CP15 treats nonzero TCM values as
  active so Armwrestler's `0x1E` ITCM mirror write works.
- ARM DSP multiply decode now uses a tight high-opcode mask so normal
  shifted-register data-processing opcodes stay in the data-processing path.
- ARM9 VRAM byte writes route to VRAM, and Engine A display mode 2 renders
  LCDC VRAM blocks into the framebuffer.
- ARM word `LDR` uses aligned-read plus rotate for unaligned addresses.
- `LDM` writeback now writes back when loading and either the base register
  is absent or it is the lowest register in the list; if the base appears
  later, the loaded value remains.

## Regression Tests

Added coverage for:

- ARM7 direct-boot load into private WRAM.
- Direct-VRAM display mode rendering from LCDC VRAM.
- ARM9 VRAM byte writes.
- ARM9 shifted-register `MOV` not being decoded as DSP multiply.
- Armwrestler-style unaligned word `LDR` rotation.
- `LDM` writeback behavior when the base register is the lowest loaded
  register, and when it is not.

## Verification

Full suite:

```sh
cargo test
```

Result after this investigation: `271 passed; 0 failed`.

Manual ROM check:

```sh
cargo run --release -p nds-frontend -- --rom test-roms/armwrestler.nds --no-audio --scale 1
```

Armwrestler now renders correctly and all tested pages pass.

## Lessons

Armwrestler is useful because it quickly separates display bring-up from
CPU correctness. A black screen was not one bug: it hid direct-boot memory
coverage, startup TCM setup, ARM decode accuracy, VRAM write routing, and
direct-VRAM rendering. Once the ROM could render its own diagnostics, the
remaining failures became precise CPU-semantic tests.

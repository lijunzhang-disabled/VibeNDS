# ARM9 CPU test fixes

Date: 2026-06-01
Status: **Fixed**

## Symptom

Before returning to the HeartGold boot problem, we reran the NDS-side CPU test
ROMs to make sure the ARM9 path was not hiding basic correctness bugs.

`arm7wrestler.nds` sampled cleanly across the menu pages, but
`armwrestler.nds` exposed two ARM9 failures:

- THUMB `LDR/STR TEST 1`: two `LDR` rows reported `BAD Rd`.
- ARM V5TE tests: `SMLABB`, `SMLABT`, `SMLATB`, and `SMLATT` reported
  `BAD Rd`.

The sibling GBA emulator passes the jsmolka GBA ROMs, but those `.gba` files
cannot currently run through this NDS direct-boot path because they are parsed
as invalid NDS headers. The useful local NDS CPU checks are therefore
`armwrestler.nds` and `arm7wrestler.nds`.

## Root Cause

Two ARM9-specific behaviors were wrong:

1. THUMB word loads were force-aligning unaligned `LDR` results on ARM9.
   ARM9 ARM-mode `LDR` already rotated aligned-word reads for unaligned
   addresses, but the THUMB register-offset, immediate-offset, and SP-relative
   word-load paths returned the raw aligned word when `is_arm9` was true.

2. `SMLAxy` treated signed overflow as a saturating write to `Rd`.
   ARMv5TE `SMLAxy` sets CPSR.Q on signed overflow, but the destination gets
   the wrapped 32-bit addition result.

## Fix

- Changed THUMB word `LDR` paths to always rotate the aligned word by
  `(addr & 3) * 8`, matching the existing ARM `LDR` behavior.
- Changed `SMLAxy` to use wrapping signed addition for `Rd` and set Q only
  when that addition overflows.
- Added unit tests for:
  - ARM9 THUMB immediate-offset unaligned `LDR`;
  - ARM9 THUMB register-offset unaligned `LDR`;
  - `SMLAxy` overflow wrapping `Rd` while setting Q.

## Verification

Targeted tests:

```sh
cargo test -p nds-core test_thumb_arm9_unaligned
cargo test -p nds-core test_smlaxy_overflow_wraps_result_and_sets_q
```

Full core suite:

```sh
cargo test -p nds-core
```

Latest result: `332 passed; 0 failed`.

ROM-level checks with the temporary frame probe:

- `armwrestler.nds` THUMB `LDR/STR TEST 1` now shows green `OK` rows.
- `armwrestler.nds` ARM V5TE tests now show green `OK` rows.
- `arm7wrestler.nds` sampled pages stayed green.

This clears the concrete CPU-test failures found before continuing the
HeartGold boot investigation.

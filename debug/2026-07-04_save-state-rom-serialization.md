# Save states were slow — the ROM was serialized twice into every state

Date: 2026-07-04
Status: **Fixed** (F5/F8 and harness states: 278.7 MB → 6.9 MB, ~85× faster)

## Symptom

F5 (save state) and F8 (load state) cause a visible, near-second hitch.

Note the distinction from **in-game** saving: HGSS's "Saving a lot of
data…" takes a few seconds on real hardware too (hundreds of flash page
programs plus read-verify over a clocked SPI bus) — that part of the
experience is authentic. The save-state hitch was not.

## Root cause

`bincode::serialize(&Nds)` included the full cartridge ROM **twice**:

- `Cart.rom: Option<Vec<u8>>` — a whole copy kept only for boot and two
  debug prints
- `SharedState.slot1_rom: Vec<u8>` — the copy the slot-1 read machine uses

For a 128 MB ROM that's ~256 MB of redundant, immutable data serialized,
zstd-compressed (F5) or decompressed + deserialized (F8), and written/read
as a ~97 MB `.state` file — every keypress. Measured on the HGSS boot
state: 0.17 s bincode → 278.7 MB, + 0.35 s zstd, + a ~97 MB disk write.

## Fix

- The ROM now lives in **one** place, `SharedState.slot1_rom`
  (`#[serde(skip)]`), halving runtime memory as a side effect. `Cart`
  keeps only the parsed header + `rom_len`.
- `Nds::take_rom()` / `Nds::reattach_rom()` carry the ROM across a state
  load: the SDL frontend moves it from the outgoing machine, the harness
  reattaches from its kept ROM bytes.
- Unit test `test_save_state_excludes_rom_and_reattach_restores_it`.

## Results

| | before | after |
|---|---|---|
| bincode serialize | 0.17 s / 278.7 MB | 0.002 s / 6.9 MB |
| deserialize | 0.15 s | 0.004 s |

With zstd over 6.9 MB, F5/F8 are effectively instant.

## Consequences for tooling

- **All previous `.state` files are invalid** (serde layout change) —
  regenerate with `hg_make_state.py`.
- A state loaded into a harness session that never called `load_rom` has
  no ROM attached; slot-1 reads return 0xFF. Always `load_rom` first
  (existing scripts already do).
- 668/668 nds-core tests pass; post-load execution verified (machine
  steps and renders normally after `load_state`, cart streaming works).

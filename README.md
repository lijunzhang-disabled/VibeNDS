# NDS Emulator

A Nintendo DS emulator written from scratch in Rust. Sibling project to the GBA emulator at `../gba`.

See [PLAN.md](PLAN.md) for the phased build plan and [ARCHITECTURE.md](ARCHITECTURE.md) for technical design.

## Status

Phase 1 (workspace + dual-CPU skeleton) — in progress.

## Build

```sh
cargo build --release
```

## Run

```sh
cargo run --release -p nds-frontend -- --rom path/to/game.nds
```

Optional flags:

```
--bios-arm9 PATH    Use a real ARM9 BIOS dump (else minimal HLE)
--bios-arm7 PATH    Use a real ARM7 BIOS dump
--firmware  PATH    Use a real firmware dump (else synthesized)
--save-type TYPE    Force backup type (eeprom-512b/eeprom-8k/eeprom-64k/fram-32k/flash-256k/flash-512k/flash-1m)
```

## Reference

- GBATEK: https://problemkaputt.de/gbatek.htm

# NDS sound test survey + rockwrestler scorecard

Date: 2026-07-15
Status: survey complete; rockwrestler failures filed as open fix list.

## Question

Can we run public NDS *sound* test ROMs and pass them?

## Finding: no public NDS sound test suite exists

Searched GitHub + forums. The DS scene never produced a blargg/mGBA-suite
style audio conformance ROM:

- **shonumi/gbe-plus-nds-tests** — README claims audio coverage, but the
  tree only has DMA / IRQ / Math / Memory / THUMB / Timer (source-only,
  needs devkitARM).
- **RockPolish/rockwrestler** — prebuilt ROM, but tests are CPU / IPC /
  math / memory / initial-state only (verified in src9/tests/: no audio).
- **BlocksDS SDK** — has audio *examples* (maxmod, libxm7, PSG demos), not
  pass/fail tests; you judge by ear.
- Emulator authors (melonDS et al.) verified audio against hardware
  recordings and GBATEK, not test ROMs.

Closest substitute for us: GBATEK-derived unit tests in `audio/` (pan law,
volume dividers, PSG duty ratios, noise LFSR, ADPCM table/clamping, loop
vs one-shot, SOUNDBIAS, SOUNDCNT mixer routing) + waveform sanity via the
harness `get_audio`.

## Our audio status (functional check)

Implemented: 16 channels, PCM8, PCM16, IMA-ADPCM, PSG square (ch 8-13),
noise (ch 14-15), volume/pan, master control. **Not implemented: sound
capture (SNDCAP0/1, 0x04000508+)** — needed by games that echo/reverb or
do the ch1/ch3 output loop.

Harness `get_audio` during DQM Joker cutscene music: stereo 32768 Hz,
L/R rms ~252/255, peaks ~±1280, balanced zero-crossings — real music, not
garbage/silence.

## Bonus: rockwrestler full scorecard (25 tests, prebuilt ROM)

Ran every menu entry headless (fresh boot per test — menus wrap, so blind
cursor math after B is unreliable). ARM7 status light: green throughout.

| Category | Test | Result |
|---|---|---|
| ARMv4 | CONDITION CODES | **OK** |
| ARMv5 | CLZ | **OK** |
| ARMv5 | QADD, QSUB | **OK** |
| ARMv5 | QDADD, QDSUB | **OK** |
| ARMv5 | SMULXY | **OK** |
| ARMv5 | SMLAXY | **OK** |
| ARMv5 | SMULWY | **OK** |
| ARMv5 | SMLAWY | **FAIL 003** |
| ARMv5 | SMLALXY | **OK** |
| ARMv5 | BLX | **OK** |
| ARMv5 | LDR r15, POP {r15}, LDM {r15} | **FAIL 005** |
| ARMv5 | LDM / STM | **FAIL 00A** |
| IPC | IPCSYNC | **TIMEOUT 003** |
| IPC | IPCFIFO | **OK** |
| IPC | IPCFIFO IRQ | **TIMEOUT 000** |
| DS MATH | SQRT 32 | **OK** |
| DS MATH | SQRT 64 | **OK** |
| DS MATH | DIV 32/32 | **FAIL 015** |
| DS MATH | DIV 64/32 | **FAIL 011** |
| DS MATH | DIV 64/64 | **OK** |
| MEMORY | WRAM CNT | **FAIL 00F** |
| MEMORY | VRAM CNT | **FAIL 008** |
| MEMORY | TCM | **FAIL 010** |
| INIT STATE | IPC/IRQ/CPSR | dump (needs HW reference) |
| INIT STATE | CP15 | dump (needs HW reference) |

**14 OK / 9 FAIL** (+2 reference dumps). The FAIL numbers are sub-case
indices — map them via `src9/tests/` in the rockwrestler repo
(armv5.s, division.cpp, ipcsync.cpp, ipcfifo_irq.cpp, wramcnt.cpp,
vramcnt.cpp, tcm.cpp).

## Repro

ROM: scratchpad `rockwrestler.nds` (from github.com/RockPolish/rockwrestler,
built 02/04/2023). Driver: `ndsh.py`; navigate DOWN×cat, A, DOWN×test, A
from a fresh boot each time; result text appears top-left ("OK" /
"FAIL nnn" / "TIMEOUT nnn").

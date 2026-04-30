# Concept: Inter-Processor Communication (IPC)

The NDS has two CPUs (ARM9 + ARM7) sharing a single Main RAM but otherwise running independent code. They coordinate via two completely separate hardware mechanisms living at the same I/O addresses on each CPU's side:

1. **`IPCSYNC`** — a 4-bit-each-direction "doorbell" register. Cheap, low-latency, no payload.
2. **`IPCFIFO`** — two 16-deep × 32-bit hardware FIFOs (one each direction). Used to ship actual messages.

Both raise IRQs on the receiving CPU when used correctly, so neither CPU has to poll. This document is what a Phase 4 implementer needs to know to build them correctly the first time.

Reference: GBATEK §"DS Inter Process Communication".

---

## 1. Mental model

> **The two CPUs share the same I/O *addresses* but see *different state* at each address.**

When the ARM9 reads `IPCSYNC` (`0x04000180`) it gets one set of bits; when the ARM7 reads `IPCSYNC` at the same address it gets *the mirrored set*. The hardware register is a single 16-bit shared latch with two read views; "this CPU's send is the other CPU's receive."

If you internalize that "two views over one shared latch" idea, everything else follows.

```
                   ┌──────────────────────┐
ARM9 view          │      shared latch    │          ARM7 view
of 0x04000180  ◄───┤                      ├───►  of 0x04000180
                   │  9→7 nybble (HI)     │
                   │  7→9 nybble (LO)     │
                   └──────────────────────┘
```

The same idea applies to `IPCFIFO` but with two real, distinct queues:

```
ARM9 SEND  (0x04000188) ──► [ FIFO_9to7 (16 × 32 bit) ] ──► ARM7 RECV (0x04100000)
ARM7 SEND  (0x04000188) ──► [ FIFO_7to9 (16 × 32 bit) ] ──► ARM9 RECV (0x04100000)
```

Note that **the RECV port is at `0x04100000`, not `0x04000184`**. This is a frequent source of confusion because nothing else in the I/O map jumps that far. It's a deliberate hardware choice so reads and writes at "the same offset" don't collide on the bus.

---

## 2. `IPCSYNC` — 0x04000180 (16 bits, both buses)

```
F  E  D  C  B  A  9  8     7  6  5  4  3  2  1  0
┌──┬──┬──┬──┬──┬──┬──┬──┐ ┌──┬──┬──┬──┬──┬──┬──┬──┐
│ 0│ E│ T│ ?│   send_data │ ?│ ?│ ?│ ?│   recv_data │
└──┴──┴──┴──┴──┴──┴──┴──┘ └──┴──┴──┴──┴──┴──┴──┴──┘
       │  │  └─ bits 11..8: data this CPU is *sending* (R/W)
       │  └──── bit 13:    "send IRQ to OTHER CPU now" (W only, self-clears)
       └─────── bit 14:    enable receive IRQ on THIS CPU
                            (when other CPU writes bit 13)
       bits 7..0: read-only — the other CPU's send_data
```

### Software semantics

- **Read** returns: `(my_send << 8) | other_send`. `recv_data` is the other CPU's `send_data` field.
- **Write** of `send_data` updates only my half (other CPU sees it on its next read in their `recv_data` slot).
- **Write** of bit 13 (`T` = trigger): *if* the other CPU has bit 14 (`E`) set, raise the IPC-Sync IRQ on the *other* CPU. Bit 13 doesn't latch — it pulses.
- The `recv_data` half is read-only from this CPU's view.

### Typical use

`IPCSYNC` is used for cheap state-machine handshakes during boot, where the actual payload is tiny enough to fit in 4 bits or where you just want to say "I'm done with phase X". Both BIOSes use it during the boot handshake to signal "ARM7 is alive, what do I do?" → "load this binary" → "ok, jumping in".

```
ARM9                                 ARM7
────                                 ────
write SYNC[hi]=0x1                   wait for SYNC[lo]==0x1
write SYNC[bit 13]=1  (kick IRQ) ──► IRQ fires, handler reads SYNC[lo],
                                      writes SYNC[hi]=0xA (ack), kicks back
wait for SYNC[lo]==0xA   ◄──────────  write SYNC[bit 13]=1
... continue boot ...
```

### Implementation contract

```rust
// SharedState already owns these — Phase 4 just wires the I/O dispatch.
pub struct IpcSync {
    pub arm9_send: u8,    // 4 bits: ARM9 → ARM7
    pub arm7_send: u8,    // 4 bits: ARM7 → ARM9
    pub arm9_recv_irq_en: bool,  // bit 14 from ARM9's writes
    pub arm7_recv_irq_en: bool,
}
```

ARM9 read of `0x04000180` returns `(self.arm9_send << 8) | self.arm7_send | (arm9_recv_irq_en << 14)`. ARM7 read returns the symmetric thing. Bit 13 writes are processed inside `write_io16` and never stored — they synchronously call `irq.request(Irq::IpcSync)` on the *other* CPU's controller.

---

## 3. `IPCFIFOCNT` — 0x04000184 (16 bits, both buses)

This is the control register. The two FIFOs are 16 entries × 32 bits each — one in each direction.

```
F  E  D  C  B  A  9  8     7  6  5  4  3  2  1  0
┌──┬──┬──┬──┬──┬──┬──┬──┐ ┌──┬──┬──┬──┬──┬──┬──┬──┐
│EN│ ?│ER│RI│ ?│ ?│RF│RE│ │ ?│ ?│ ?│SC│SI│ ?│SF│SE│
└──┴──┴──┴──┴──┴──┴──┴──┘ └──┴──┴──┴──┴──┴──┴──┴──┘
 │     │  │     │  │       │  │
 │     │  │     │  │       │  └─ bit 0:  send-FIFO empty (RO)
 │     │  │     │  │       └──── bit 1:  send-FIFO full  (RO)
 │     │  │     │  │
 │     │  │     │  └────── bit 8:  recv-FIFO empty (RO)
 │     │  │     └───────── bit 9:  recv-FIFO full  (RO)
 │     │  │
 │     │  └────── bit 10: recv-FIFO not-empty IRQ enable (RW)
 │     └───────── bit 14: error flag — set sticky on under/overflow (R; W=1 clears)
 └─────────────── bit 15: master enable for the whole pair (RW)
                          + bit 3:  send-FIFO empty IRQ enable (RW)
                          + bit 2:  send-FIFO clear (W=1 to flush this CPU's outgoing)
```

### The four IRQs the pair can raise

| Bit | Trigger | Raised on |
|---|---|---|
| 3 (`SI`) | This CPU's *send* FIFO becomes empty | This CPU |
| 10 (`RI`) | This CPU's *recv* FIFO becomes non-empty | This CPU |
| (none) | Other CPU sets `IPCSYNC` bit 13 | Other CPU |
| (none) | Other CPU writes `SI` rising-edge while own send is empty | (no IRQ — that's the same as the first row from the *other* side) |

The "send-empty" IRQ is what producers use to know they can safely push more without overflowing — when their send FIFO drains to zero, the hardware nudges them.

The "recv-not-empty" IRQ is what consumers use to wake up — they don't have to poll, the FIFO writing CPU effectively wakes them.

### Master enable and clear

- `EN` (bit 15) = master enable. While 0, both FIFOs reject writes and reads return 0. Real games set this once at boot. We set it on whatever value the game writes; we don't enforce it for sends (most homebrew leaves it set the whole time).
- `SC` (bit 3, **on writes** — not the same as `SI` which is also bit 3 in the read view, slightly confusing) clears the send FIFO when written 1. Hardware: `SC` is at write-bit-3, `SI` IRQ-enable is at the same position read-wise. **Read after write returns the IRQ-enable bit, not the cleared bit, so writes to 3 are write-only-ish.**

  Actually the cleaner read of GBATEK is that bit 3 has separate read/write semantics depending on context: the IRQ enable lives at bit 2 in the alternative numbering some docs use. We follow GBATEK's bit numbering literally.

### Error flag (`ER`, bit 14)

Set sticky when this CPU:
- writes to its send FIFO when it's full (write is dropped), or
- reads from its recv FIFO when it's empty (read returns the last value successfully popped — or 0 if never popped).

The read-empty case is the subtle one: a misbehaving consumer that reads past empty *doesn't stall* and *doesn't get a fresh value*; it gets the stale last-value and a sticky error flag. Games can read FIFO until empty and then use the flag to know they've drained.

---

## 4. `IPCFIFOSEND` — 0x04000188 (32 bits, write only)

Writes a u32 to **this CPU's send FIFO**, which is the **other CPU's recv FIFO**. If full, the write is dropped and `ER` is set.

```rust
fn write_send(side: Side, val: u32) {
    let q = match side { Arm9 => &mut fifo_9to7, Arm7 => &mut fifo_7to9 };
    if q.len() == 16 {
        // overflow
        cnt[side].error = true;
        return;
    }
    let was_empty_on_other_side = q.is_empty();
    q.push_back(val);
    // Maybe wake the other CPU:
    if was_empty_on_other_side && cnt[other(side)].recv_irq_en {
        irq[other(side)].request(IpcRecvNotEmpty);
    }
}
```

The "was_empty → recv-not-empty IRQ" check fires *only on the empty→non-empty transition*. That's a very specific hardware quirk: pushing the 2nd, 3rd, ... 16th word doesn't re-raise the IRQ; the consumer is expected to drain everything once woken. Get this wrong and games either spin (no IRQ) or storm (re-raised every push).

## 5. `IPCFIFORECV` — 0x04100000 (32 bits, read only)

Reads (and pops) the head of **this CPU's recv FIFO**. If empty, returns either the last successfully popped value or 0 (we choose 0 for simplicity), and sets `ER`.

```rust
fn read_recv(side: Side) -> u32 {
    let q = match side { Arm9 => &mut fifo_7to9, Arm7 => &mut fifo_9to7 };
    if q.is_empty() {
        cnt[side].error = true;
        return last_popped[side];
    }
    let val = q.pop_front().unwrap();
    last_popped[side] = val;
    // Maybe signal the other CPU's "send empty":
    if q.is_empty() && cnt[other(side)].send_empty_irq_en {
        irq[other(side)].request(IpcSendEmpty);
    }
    val
}
```

Same trick: send-empty IRQ fires on the *non-empty → empty* transition, only.

---

## 6. Who sees which queue

This trips up everyone the first time. Pin it on the wall:

| Operation | ARM9 perspective | ARM7 perspective |
|---|---|---|
| Write `0x04000188` | pushes to **`fifo_9to7`** | pushes to **`fifo_7to9`** |
| Read `0x04100000`  | pops from **`fifo_7to9`** | pops from **`fifo_9to7`** |
| `IPCFIFOCNT.SE` (bit 0)  | empty status of **`fifo_9to7`** | empty status of **`fifo_7to9`** |
| `IPCFIFOCNT.RE` (bit 8)  | empty status of **`fifo_7to9`** | empty status of **`fifo_9to7`** |
| Send IRQ (bit 17)  | raises on **ARM9** when **`fifo_9to7`** drains | raises on **ARM7** when **`fifo_7to9`** drains |
| Recv IRQ (bit 18)  | raises on **ARM9** when **`fifo_7to9`** transitions empty→non-empty | raises on **ARM7** when **`fifo_9to7`** transitions empty→non-empty |

There is **no scenario** where ARM9 looking at "my send FIFO's empty bit" sees the same bit as ARM7 looking at "my send FIFO's empty bit" — the bits are at the same I/O *position* but report different queues.

---

## 7. Typical usage patterns

These are the patterns commercial games and homebrew use. Implementing IPC correctly means each pattern works without games having to know our emulator exists.

### 7.1 Boot handshake (BIOS)

ARM9 BIOS spins on `IPCSYNC.recv == 0x1`, ARM7 BIOS spins on `IPCSYNC.recv == 0x1`. They use the doorbell to advance through ~6 handshake states (chip-ID exchange, encryption seeds, "you're booted, jump"). Direct boot bypasses all of this — we set up registers and PC manually — but a real-BIOS path will need full IPCSYNC fidelity.

### 7.2 Task dispatch (most games)

ARM9 owns the cart, GPU, audio mixer setup. It posts "tasks" to ARM7 via the FIFO:

- ARM9 → ARM7: `cmd_word = (cmd_id << 24) | payload`. Common cmd_ids are reset, sound channel control, touch-coords-please, slot-1 read.
- ARM7 → ARM9: response with the same `cmd_id` echoed back, plus result.

The receiving CPU has its `RI` enabled, so it never polls — the FIFO write itself wakes it. The producer doesn't have to wait for "send empty" because it just pushes one or two words per task.

### 7.3 Streaming (e.g. firmware/touch reads)

ARM9 wants 16 ADC samples averaged. It pushes one command, then sleeps on `RI`. ARM7 pushes 16 reply words. The empty→non-empty IRQ fires *once* (on the first reply word). ARM9's handler then drains until `SE` of its recv FIFO is set.

### 7.4 Audio sample upload (libnds-style)

ARM9 builds a sound buffer in Main RAM, pushes its address to ARM7 via FIFO, ARM7's sound engine starts playing from that pointer. Pure pointer-passing — payload is the address, not the data.

---

## 8. Edge cases worth preserving

| Quirk | Why it matters |
|---|---|
| Read of empty recv returns last successfully popped value | Some homebrew detects empty by reading until the value stops changing. Our impl returns 0 instead — explicitly different from real hardware but simpler; flag it in `debug/` if a game depends on stale-read. |
| IRQ fires only on the empty↔non-empty *transition* | Repeatedly pushing N words must not raise N IRQs. Repeatedly popping until empty must raise send-empty exactly once. |
| Master enable (`EN`, bit 15) gates EVERYTHING | Reset puts it 0 = both FIFOs frozen. The BIOS / direct boot must set it before any FIFO traffic is meaningful. |
| `IPCSYNC` send-data is 4-bit-only | Bits above 11..8 in `send_data` are ignored on write. Don't accidentally store 8 bits. |
| Trigger bit 13 doesn't latch | Reads of `IPCSYNC.13` should always return 0. Writes pulse the IRQ regardless of the bit's current value. |
| `IPCSYNC` IRQ enable bit (14) is *this CPU's* receive enable | Easy to mix up: the IRQ raised by this CPU's bit-13 write goes to the *other* CPU, but the *other* CPU's bit-14 setting decides whether to deliver it. We store both halves and consult the right one at trigger time. |

---

## 9. Implementation plan for Phase 4

### Data layout (in `SharedState`)

```rust
pub struct Ipc {
    // Sync
    pub sync_arm9_send: u8,           // 4-bit
    pub sync_arm7_send: u8,           // 4-bit
    pub sync_arm9_recv_irq_en: bool,
    pub sync_arm7_recv_irq_en: bool,

    // FIFOs
    pub fifo_9to7: VecDeque<u32>,     // bounded to 16
    pub fifo_7to9: VecDeque<u32>,
    pub last_popped_9: u32,
    pub last_popped_7: u32,

    // Per-CPU FIFOCNT
    pub fifo_arm9_enable: bool,
    pub fifo_arm7_enable: bool,
    pub fifo_arm9_send_empty_irq: bool,
    pub fifo_arm9_recv_irq: bool,
    pub fifo_arm9_error: bool,
    pub fifo_arm7_send_empty_irq: bool,
    pub fifo_arm7_recv_irq: bool,
    pub fifo_arm7_error: bool,
}
```

### Routing

- `IPCSYNC` reads/writes go through the existing `io_arm9` / `io_arm7` dispatchers. Whoever's writing knows which "side" they are; the helpers take a `Side` enum.
- `IPCFIFOCNT`, `IPCFIFOSEND`, `IPCFIFORECV` likewise.

### IRQ raising

When ARM9 writes `IPCSYNC` bit 13 *and* `arm7_recv_irq_en` is set, we call `shared.irq7.request(Irq::IpcSync)` immediately (synchronously inside the write helper). Same for the other direction. We're using lockstep at 1 ARM7 cycle, so the IRQ is visible on the *next* ARM7 step — no scheduler event needed.

For the FIFO transition IRQs: same pattern. The push helper checks the empty→non-empty edge after the push and calls `irq.request` directly.

### Bounded queues

We back each FIFO with a `VecDeque<u32>` and explicitly check `len() == 16` before push. We do **not** use a `Vec` without a bound — if a game leaks pushes (e.g. through an emulation bug elsewhere) we want to detect overflow with `error = true`, not OOM.

### Tests we need

- IPCSYNC write-then-read round trip in both directions; bit 13 trigger raises IPC-Sync IRQ on the receiver only when the receiver has bit 14 set.
- FIFO 16-word push fills, 17th sets error flag, doesn't overwrite.
- FIFO push raises recv-not-empty IRQ once per empty→non-empty transition.
- FIFO pop on empty returns last-popped, sets error.
- FIFO pop down to empty raises send-empty IRQ on the *other* CPU.
- Master enable gates: with `EN=0`, pushes are dropped, pops return 0.
- Two-CPU integration test: ARM9 pushes 4 words; ARM7's IRQ handler reads them in order.

### What's *not* in scope for IPC alone

- The lockstep granularity stays at 1 ARM7 cycle for now (locked decision). After Phase 4 verifies IPC is correct end-to-end, we can revisit coarser interleaving — that's where IPC-correct-but-emulator-fast tradeoffs live.
- BIOS HLE handlers that *use* IPC (e.g. ARM7-side touch-coordinate fetch) are wired in Phase 5 once SPI is available.

---

## 10. Things I'm explicitly choosing wrong-but-simpler (to flag if a game breaks)

1. **Trigger IRQs synchronously** inside the I/O write helper. Real hardware has a small latency between bit-13 write and IRQ assertion on the other side; we collapse that to instant. With lockstep stepping at 1 ARM7 cycle the difference is sub-instruction and shouldn't matter to any game.
2. **No "FIFO half-full" signal** — there isn't one in NDS hardware (that's a GBA thing for sound DMA). The empty / not-empty / full bits are exhaustive.
3. **Bit-13 of `IPCSYNC` reads as 0 always**. That matches GBATEK; some emulators preserve the last-written value but no game checks.

We *do* implement read-empty-returns-last-popped (real hardware behavior) — `last_popped_9` / `last_popped_7` in the struct above carry it.

If any of these turn out to bite us, the fix is local — adjust the helper, add a regression test under `debug/`, and move on.

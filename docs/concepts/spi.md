# Concept: SPI bus

SPI (**Serial Peripheral Interface**) is a synchronous serial protocol invented by Motorola in the 1980s. It's the simplest "talk to a chip" protocol that's still in widespread use today: 4 wires, no addressing scheme on the wire, fully software-defined per-peripheral. The NDS uses SPI in two completely separate places — one for system-side peripherals (firmware, touchscreen, power management) and one for cart-side backup. This doc covers what SPI is and how we model both.

## 1. The wires

```
master (CPU side)                       slave (peripheral)
─────────────────                       ──────────────────

SCK   ──── clock ─────────────────►     SCK     (slave samples on edges)
MOSI  ──── master-out, slave-in ──►     MOSI    (data master → slave)
MISO  ◄─── master-in,  slave-out ──     MISO    (data slave → master)
CS    ──── chip select (active-low) ►   CS      (slave listens iff CS = low)
```

The master drives the clock. On every clock edge, **both** sides shift one bit at the same time: master shifts a bit out on MOSI and samples a bit in on MISO. After 8 edges, a byte has flowed in each direction — SPI is **full-duplex by definition**, every byte sent is also a byte received.

This is why our `SpiBus::write_data(byte_in)` returns a byte: the CPU writes "shift this byte out" and simultaneously gets "here's the byte that came back from the slave."

## 2. Multi-device topology

CS is the one wire that's *not* shared. Multiple slaves can sit on the same SCK/MOSI/MISO triple as long as each has its own CS wire to the master:

```
                   ┌─ slave 1 (firmware) ─┐
                   │   SCK MOSI MISO CS₁  │
master ─── SCK ────┤                       │
            MOSI ──┤                       │
            MISO ──┤   (MISO tri-stated    │
            CS₁ ───┘    unless CS₁ low)    │
            CS₂ ───┐                       │
                   │   SCK MOSI MISO CS₂   │
                   ├─ slave 2 (TSC) ───────┘
            CS₃ ───┤
                   │   SCK MOSI MISO CS₃
                   └─ slave 3 (PMIC)
```

The master picks one slave at a time by pulling its CS line low. Only that slave drives MISO; others go high-impedance. Bus arbitration is trivial because there is none — the master alone decides who's talking.

On the NDS, the "CS line picker" is `SPICNT[9:8]` (device select). Three slaves share the bus: PMIC, firmware, TSC.

## 3. How a transaction works

SPI defines no protocol above "shift bytes." Each peripheral defines its own command vocabulary. The shape is almost always:

```
1. Assert CS                                (master)
2. Send command byte                        (master → slave)
3. Send any address bytes the command needs (master → slave)
4. Send dummy bytes to clock out the result (master → slave),
   while reading the data slave is shifting back (slave → master)
5. Deassert CS                              (master)
```

Concrete trace — ARM7 reading 2 bytes from firmware offset `0x000010`:

```
Step   MOSI       MISO   meaning
─────  ────       ────   ───────
CS↓                       — master pulls CS low
 1     0x03       0      READ command
 2     0x00       0      addr [23:16]
 3     0x00       0      addr [15:8]
 4     0x10       0      addr [7:0]
                          — slave is now in "streaming data" state
 5     0x00       byte0  master sends dummy; slave shifts back data[0x10]
 6     0x00       byte1  master sends dummy; slave shifts back data[0x11]
CS↑                       — slave resets its FSM; transaction over
```

Two important details:

- **The slave's bytes on MISO during steps 1-4 are garbage** (usually 0). The slave hasn't finished decoding the command yet. Software discards them.
- **The dummy bytes on MOSI during steps 5-6 are filler**. Their value doesn't matter; the master only sends them because that's how it clocks bytes out of the slave.

## 4. CS-hold semantics — the load-bearing detail

Real SPI hardware exposes a "hold CS asserted between bytes" control. Without it, the master would have to issue all of a transaction's bytes as one atomic SPI operation. With it, the master can clock one byte, do CPU stuff, clock the next byte, do more CPU stuff, etc — as long as CS stays low, the slave keeps its state.

On NDS, `SPICNT[11]` is the hold bit:

- `hold = 1` on a byte → "more bytes coming, keep CS low after this one"
- `hold = 0` on a byte → "this is the last byte; deassert CS after"

CS deassert is what **resets the slave's FSM**. Get this wrong and the slave is half-stuck in a previous transaction's state — silently breaking the next one.

This is why our test scaffolding obsesses over the hold sequence:

```rust
let cnt_hold = (1 << 15) | (2 << 8) | (1 << 11);  // enable + TSC + hold
let cnt_drop = (1 << 15) | (2 << 8);              // enable + TSC, hold=0

bus.cnt = cnt_hold; bus.write_data(0x80 | (5 << 4)); // byte 1: control, hold
bus.cnt = cnt_hold; bus.write_data(0);               // byte 2: read hi, hold
bus.cnt = cnt_drop; bus.write_data(0);               // byte 3: read lo, drop CS
```

If you mistakenly send the control byte with `hold = 0`, the SPI bus immediately calls `tsc.reset()` and discards the control state — the next two bytes read zeros forever.

## 5. Why NDS uses SPI

Pin economy. The ARM7 has to talk to four different chips:

- **Firmware** — 256 KB flash with user settings, calibration, WiFi cal
- **TSC (touchscreen controller)** — 12-bit ADC
- **PMIC (power management IC)** — backlight, sound enable, battery
- **Cart backup** — separately via AUXSPI (more on this below)

If each had a parallel bus, that's 4 chips × ~12 pins each = ~48 wires. With SPI, MOSI/MISO/SCK are shared and only CS is per-chip — **4 chips × 1 dedicated wire + 3 shared = 7 wires**. Saves board area, saves power, lets Nintendo source off-the-shelf chips that already speak SPI.

The trade-off: SPI is slow. Each byte takes 8 SCK cycles, no parallelism, no DMA-bulk transfer. But these are all low-bandwidth devices: firmware is read once at boot, TSC samples 100 Hz, PMIC settings change rarely. Slow is fine.

## 6. SPI vs a parallel memory bus

| | Memory bus (Main RAM, GBA Flash) | SPI bus |
|---|---|---|
| Wires | 32 data + ~24 address + control | 4 (SCK / MOSI / MISO / CS per slave) |
| Addressing | Hardware decodes address → drives one chip | None on the wire; commands embed addresses |
| Bandwidth | Hundreds of MB/s | Hundreds of KB/s |
| Latency per access | 1-10 cycles | ~80 cycles (10 SCK ticks × N bytes) |
| Multi-device on one bus | Yes, address-decoded | Yes, CS-selected (one slave at a time) |
| Bus arbitration | Real (DMA vs CPU) | None — master controls everything |
| Per-device protocol | Same (load / store) | Each slave defines its own |

SPI is the right tool when "many low-bandwidth peripherals, few pins" is the constraint. A memory bus is the right tool when "one high-bandwidth memory, many wires OK" is.

## 7. How we model it

`spi/mod.rs::SpiBus` is the dispatcher. The pertinent piece:

```rust
pub fn write_data(&mut self, byte_in: u8) -> bool {
    let device = Device::from_bits(self.cnt);   // bits 8:9 of SPICNT pick the slave
    let hold = self.cnt & (1 << 11) != 0;       // bit 11 = CS hold

    let byte_out = match device {                // shift one byte both ways
        Device::Pmic     => self.pmic.xfer(byte_in, hold),
        Device::Firmware => self.firmware.xfer(byte_in, hold),
        Device::Tsc      => self.tsc.xfer(byte_in, hold),
        Device::Reserved => 0xFF,
    };
    self.data = byte_out;

    if !hold {
        self.reset_device(device);               // CS deassert: reset slave FSM
    }
    self.cnt & (1 << 14) != 0                    // transfer-complete IRQ enable?
}
```

Each slave implements `xfer(byte_in, hold) -> byte_out` plus `reset()`. The slave is modelled as a small enum-discriminated state machine where each `xfer` advances one state and `reset()` snaps back to `Idle`. Example shape from `spi/firmware.rs`:

```rust
enum Phase {
    Idle,                                                // awaiting command
    AddressBytes { cmd: u8, remaining: u8, addr: u32 },  // collecting addr bytes
    Data { cmd: u8, addr: u32 },                         // streaming data
}
```

The Phase transitions literally correspond to "what byte type is this slave expecting next?" — which is exactly what a real SPI peripheral's silicon FSM does.

## 8. The two SPI buses on NDS

The NDS has **two** physically separate SPI buses. They use the same protocol but talk to different peripherals via different register pages:

| Bus | Register page | Slaves | Code module |
|---|---|---|---|
| Main SPI | `0x040001C0..C3` (`SPICNT`, `SPIDATA`) | PMIC (dev 0), firmware (dev 1), TSC (dev 2) | `spi/` |
| AUXSPI | `0x040001A0..A3` (`AUXSPICNT`, `AUXSPIDATA`) | Cart backup chip (one slave) | `cart/auxspi.rs` |

Both are ARM7-side by default. AUXSPI shares the slot-1 control word `AUXSPICNT` with the slot-1 ROM transfer machine — bit 13 of that register picks "ROM transfer mode" (0) vs "AUXSPI backup mode" (1). Same physical pins reused two ways.

The reason for the split: cart backup chips are vendor-supplied with the game cartridge, so they need their own bus that's accessible without going through any console-internal peripheral mux. AUXSPI is literally a couple of extra pins on the cart slot.

## 9. Per-slave protocols (very quick tour)

### Firmware (`spi/firmware.rs`)

Standard SPI flash command set:

| Cmd | Name | Phase after sending |
|---|---|---|
| `0x03` | READ | 3 addr bytes → stream data |
| `0x05` | READ_STATUS | stream status byte (repeats) |
| `0x06` | WRITE_ENABLE | latch WEL = 1, done |
| `0x04` | WRITE_DISABLE | latch WEL = 0, done |
| `0x9F` | READ_JEDEC_ID | stream 3 ID bytes |
| `0x0A` | PAGE_PROGRAM | 3 addr bytes → stream data to write |
| `0xD8` | SECTOR_ERASE | 3 addr bytes → erase fires on last addr byte |

256 KB image; user settings at `0x3FE00` and `0x3FF00` (two copies, BIOS picks the higher counter). We synthesize a default settings block on startup so games that read nickname / language / calibration don't crash without a real firmware dump.

### TSC (`spi/tsc.rs`)

ADS7843-style 12-bit ADC. Every conversion is exactly **3 bytes** on the bus:

```
byte 1: 0b 1 cc cc cc m r pp     // start | channel | 12-bit/8-bit | ref | power
byte 2: high 7 bits of result (top bit padded 0)   ← read here
byte 3: low 5 bits of result (shifted to top of byte) ← read here
```

Channels: 1=Y, 2=battery, 3=Z1, 4=Z2, 5=X, 6=AUX, 7=temperature.

Software wraps `Nds::set_touch(x, y, pen_down)` calls; the TSC then linearly maps screen coords to ADC values that match the firmware calibration block, so a game's calibrated conversion produces the exact pixels the frontend fed in.

### PMIC (`spi/pmic.rs`)

Minimal 8-register chip. Each transaction is **2 bytes**: address byte (bit 7 = direction), data byte. Reads return the stored register; writes store it. Registers we care about:

- Reg 0: control (sound enable, top/bottom backlight, power-off)
- Reg 4: battery status (we return "OK")

### AUXSPI (`cart/auxspi.rs`)

Backup chip on the cart. Two protocol families share the same command bytes:

- **EEPROM / FRAM** — 0x03 READ + 0x02 WRITE with 1/2/3-byte addresses depending on size class (512 B → 1 byte, 8 KB → 2 bytes, 64 KB → 3 bytes).
- **FLASH** — adds 0x0A PAGE_PROGRAM (cells only flip 1→0 per real NAND/NOR semantics; you need an erase to flip 0→1) and 0xD8 SECTOR_ERASE (4 KB sectors fill with 0xFF).

All modifying ops gate on the WEL latch (set by `0x06 WRITE_ENABLE`).

## 10. The mental model

> **SPI is a "shift-register dialogue" with CS-bounded transactions.** Every clock edge = one bit each direction. Every byte exchange = one step in a multi-byte command protocol that the slave defines unilaterally. Dropping CS resets the slave's FSM back to "awaiting command."

That's also the shape of every slave in our codebase: a Phase enum, a transition function `xfer`, and a `reset` that snaps to Idle. Every test we write for an SPI slave is a sequence of `xfer` calls with the right hold pattern. Get the hold sequence right and the slave behaves; get it wrong and the slave silently sits in the wrong state.

This is why each of our SPI device modules has the same skeleton (`Phase` enum + `xfer` + `reset`) — once you've internalized the SPI mental model, every new slave is a 100-200 line port-of-the-state-machine job, not a new architecture problem.

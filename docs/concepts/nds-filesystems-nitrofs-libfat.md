# NDS filesystem concepts — NitroFS, Slot-1, and DLDI/libfat

Date: 2026-06-01
Status: **Reference**

## Short Version

Yes: the built-in filesystem used by many NDS homebrew and SDK-style programs
is usually called **NitroFS**. It is not the same thing as FAT/libfat.

The main hardware reason is that **GBA cartridges are external memory, while
DS cards are storage devices**. That one design shift explains most of the
difference between GBA ROM emulation and NDS Slot-1 emulation.

Small correction: DS Slot-1 is not purely serial. It has an 8-bit data bus
(`D0-D7`) plus clock, chip select, reset, and related control pins. But unlike
the GBA cart, it is not memory-mapped as directly addressable ROM. The DS talks
to Slot-1 through the game-card controller: send an 8-byte command, then receive
a stream of data. GBATEK describes DS cart ROM access as 8-byte commands
followed by a data stream, and notes that cartridge memory must be copied to RAM
because the CPU cannot execute directly from ROM. The DS game-card pinout shows
the same model: `D0-D7`, `CLK`, chip select, reset, and command/data transfers
framed by chip-select transitions.

On the NDS there are two common filesystem paths an emulator needs to care
about:

- **NitroFS**: files embedded inside the `.nds` ROM image and read through the
  cart/Slot-1 ROM interface.
- **libfat via DLDI**: files on a writable block device, historically a flash
  cart SD card, accessed through a DLDI driver and mounted as a FAT volume.

Those paths exercise different emulator hardware.

## GBA Cart Versus DS Card

On the **GBA**, the cartridge is part of the CPU address space. Game Pak ROM
appears at `0x08000000-0x0DFFFFFF`, uses a 16-bit ROM bus, and can be read by
CPU or DMA like normal memory. This fit the GBA's small RAM budget: many games
execute substantial amounts of Thumb code directly from cartridge ROM.

On the **DS**, Nintendo changed the model. DS software is expected to load code,
overlays, and assets from card into RAM, then execute from RAM. The DS has 4 MB
main RAM plus other memories, and Slot-1 behaves like a packet/block storage
device behind a controller rather than CPU-visible ROM.

That design has several consequences:

- **Smaller card interface**: a GBA-style memory bus needs many address, data,
  and control pins. The DS kept a 32-pin GBA slot for backward compatibility,
  but Slot-1 is a compact 17-pin game-card interface.
- **Security and anti-piracy**: the DS card protocol includes chip IDs,
  secure-area reads, KEY1/KEY2 encryption state, restricted regions, and command
  modes. A command controller makes those state machines natural to place
  between the CPU and ROM storage.
- **RAM-first execution**: because the CPU cannot execute directly from Slot-1,
  commercial games copy ARM9/ARM7 code, overlays, and asset blocks into RAM
  before using them.
- **Storage abstraction**: the console asks for byte streams at ROM addresses;
  the card can hide its internal mask ROM, banking, special alignment rules,
  larger future chips, and later cartridge variations.

For emulation, the difference is concrete:

| System | Cartridge Model | Emulator Model |
|---|---|---|
| GBA | Memory chip on the CPU bus | Mapped ROM reads plus waitstates/prefetch |
| NDS | Block/packet storage behind game-card controller | Card command engine, data stream/FIFO, DMA timing, secure modes |

This is why a GBA emulator can mostly model ROM as mapped memory, while an NDS
emulator needs Slot-1 card control registers, an 8-byte command phase, streamed
`CARD_DATA_RD` reads, DMA start timing, chip-ID behavior, secure-area behavior,
KEY1/KEY2 state, and ARM9/ARM7 access ownership through `EXMEMCNT`.

## NitroFS

NitroFS is the ROM filesystem format used by the official Nitro SDK and also
supported by libnds homebrew. The `.nds` header points to two filesystem tables:

- **FNT**: file name table, the directory tree and file names.
- **FAT**: file allocation table, start/end ROM offsets for each file.

File bytes live inside the same ROM image. A program that opens
`nitro:/some/file.txt` eventually reads sectors or byte ranges from Slot-1 ROM
space, using the FNT/FAT metadata to translate the path into ROM offsets.

For this emulator, a NitroFS pass primarily proves:

- the `.nds` header is loaded correctly;
- direct boot leaves the expected header/filesystem metadata visible;
- Slot-1 ownership via `EXMEMCNT` behaves well enough;
- normal card ROM commands can read header/chip ID/main ROM data;
- reads from `CARD_DATA_RD` and `ROMCTRL` status behave coherently.

The devkitPro `filesystem/nitrofs/nitrodir` test is the current proof point: it
mounts NitroFS and lists embedded `nitro://...` files from inside the ROM.

## DLDI And libfat

libfat is different. It expects a block device containing a FAT filesystem.
On real homebrew setups this was often supplied by a flash cart. Because each
cart had different storage hardware, NDS homebrew used **DLDI**:

- DLDI means Dynamically Linked Device Interface.
- A ROM contains a DLDI stub or patched DLDI driver.
- libfat calls that driver to initialize the storage device and read/write
  512-byte sectors.

Modern devkitPro/libnds routes this through Calico block-device calls. On ARM9,
libfat/libdvm calls `blkDevInit`, `blkDevIsPresent`, `blkDevReadSectors`, and
similar functions. Those communicate with the storage provider over the PXI
block-device channel.

For this emulator, a libfat pass primarily proves:

- IPC/PXI block-device request/reply behavior is good enough;
- sector count is exposed through the Calico transfer region;
- reads and writes copy data between emulated main RAM and the block image;
- the mounted block image has a FAT boot sector/BPB layout libfat accepts.

The devkitPro `filesystem/libfat/libfatdir` test is the current proof point: it
mounts the emulator-backed FAT16 image and lists `README.TXT` plus `[GAMES]`.

## Why Both Matter

NitroFS and libfat can both look like "filesystem support" from the app's point
of view, but they prove different emulator features:

| Path | Backing Storage | Main Hardware Path | Typical URI |
|---|---|---|---|
| NitroFS | Files embedded in the `.nds` ROM | Slot-1 ROM reads | `nitro:/...` |
| libfat/DLDI | FAT volume on a block device | PXI + DLDI block sectors | `/...`, `fat:/...` |

This distinction mattered in the compatibility sweep. NitroFS passed after
Slot-1 ROM reads and direct-boot argv/header behavior were fixed. libfat still
failed until the emulator added a DLDI/PXI block-device service and a FAT16
image with a BPB layout accepted by libfat's VBR probe.

## Commercial Game Relevance

Commercial games usually do **not** use DLDI/libfat. They normally read their
own assets from the ROM through NitroFS-like SDK filesystem tables or custom
archive formats on top of Slot-1 ROM reads.

So before trying commercial games:

- NitroFS/card-read behavior is highly relevant.
- DLDI/libfat is mostly relevant to homebrew, tools, and apps that expect an
  SD-like writable filesystem.
- Save data is another separate path again: EEPROM/flash/FRAM over AUXSPI, not
  NitroFS or libfat.

## References

- [GBATEK: DS Cartridge Protocol](https://problemkaputt.de/gbatek-ds-cartridge-protocol.htm)
- [Hardware Book: DS Game Card](https://www.hardwarebook.info/DS_Game_Card)
- [GBATEK: GBA Memory Map](https://problemkaputt.de/gbatek-gba-memory-map.htm)
- [GBATEK: DS Cartridge / GBA Slot](https://problemkaputt.de/gbatek-ds-cartridge-gba-slot.htm)
- [GBATEK: DS Technical Data](https://problemkaputt.de/gbatek-ds-technical-data.htm)
- [GBATEK: DS NitroROM and NitroARC File Systems](https://problemkaputt.de/gbatek-ds-cartridge-nitrorom-and-nitroarc-file-systems.htm)
- [BlocksDS: Filesystem Support](https://blocksds.skylyrac.net/docs/guides/filesystem/)

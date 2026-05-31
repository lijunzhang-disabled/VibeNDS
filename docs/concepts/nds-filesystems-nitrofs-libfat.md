# NDS filesystem concepts — NitroFS, Slot-1, and DLDI/libfat

Date: 2026-06-01
Status: **Reference**

## Short Version

Yes: the built-in filesystem used by many NDS homebrew and SDK-style programs
is usually called **NitroFS**. It is not the same thing as FAT/libfat.

On the NDS there are two common filesystem paths an emulator needs to care
about:

- **NitroFS**: files embedded inside the `.nds` ROM image and read through the
  cart/Slot-1 ROM interface.
- **libfat via DLDI**: files on a writable block device, historically a flash
  cart SD card, accessed through a DLDI driver and mounted as a FAT volume.

Those paths exercise different emulator hardware.

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

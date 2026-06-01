use super::SharedState;
use crate::interrupt::Irq;

const PXI_CHANNEL_BLKDEV: u32 = 2;
const PXI_CHANNEL_EXTENDED: u32 = 31;
const PXI_DIR_RESPONSE: u32 = 1 << 5;

const BLK_MSG_IS_PRESENT: u32 = 0;
const BLK_MSG_INIT: u32 = 1;
const BLK_MSG_READ_SECTORS: u32 = 2;
const BLK_MSG_WRITE_SECTORS: u32 = 3;

const DLDI_DEV: u32 = 0;
const SECTOR_SIZE: usize = 512;
const DLDI_SECTORS: usize = 32 * 1024;
const TRANSFER_SECTOR_COUNT_ADDR: u32 = 0x02FF_FF80;

pub fn service_pxi(shared: &mut SharedState) {
    loop {
        if !try_service_one(shared) {
            break;
        }
    }
}

pub fn has_pending_request(shared: &SharedState) -> bool {
    let Some(&packet) = shared.ipc.fifo_9to7.front() else {
        return false;
    };

    if packet_channel(packet) == PXI_CHANNEL_BLKDEV && !packet_is_response(packet) {
        let imm = packet >> 6;
        let ty = imm & 0x1f;
        let dev = (imm >> 5) & 0x7ff;
        return dev == DLDI_DEV && matches!(ty, BLK_MSG_IS_PRESENT | BLK_MSG_INIT);
    }

    if packet_channel(packet) == PXI_CHANNEL_EXTENDED && !packet_is_response(packet) {
        let ext_ch = (packet >> 6) & 0x1f;
        let imm = packet >> 16;
        let ty = imm & 0x1f;
        let dev = (imm >> 5) & 0x7ff;
        return ext_ch == PXI_CHANNEL_BLKDEV
            && dev == DLDI_DEV
            && matches!(ty, BLK_MSG_READ_SECTORS | BLK_MSG_WRITE_SECTORS);
    }

    false
}

fn try_service_one(shared: &mut SharedState) -> bool {
    let Some(&packet) = shared.ipc.fifo_9to7.front() else {
        return false;
    };

    if packet_channel(packet) == PXI_CHANNEL_BLKDEV && !packet_is_response(packet) {
        let imm = packet >> 6;
        let ty = imm & 0x1f;
        let dev = (imm >> 5) & 0x7ff;
        if dev != DLDI_DEV || !matches!(ty, BLK_MSG_IS_PRESENT | BLK_MSG_INIT) {
            return false;
        }

        shared.ipc.fifo_9to7.pop_front();
        let reply = match ty {
            BLK_MSG_IS_PRESENT => 1,
            BLK_MSG_INIT => {
                ensure_image(shared);
                write_transfer_sector_count(shared);
                1
            }
            _ => 0,
        };
        push_reply(shared, reply);
        raise_send_empty_if_needed(shared);
        return true;
    }

    if packet_channel(packet) != PXI_CHANNEL_EXTENDED || packet_is_response(packet) {
        return false;
    }

    let ext_ch = (packet >> 6) & 0x1f;
    if ext_ch != PXI_CHANNEL_BLKDEV {
        return false;
    }

    let word_count = (((packet >> 11) & 0x1f) + 1) as usize;
    if shared.ipc.fifo_9to7.len() < 1 + word_count {
        return false;
    }

    let imm = packet >> 16;
    let ty = imm & 0x1f;
    let dev = (imm >> 5) & 0x7ff;
    if dev != DLDI_DEV || !matches!(ty, BLK_MSG_READ_SECTORS | BLK_MSG_WRITE_SECTORS) {
        return false;
    }

    shared.ipc.fifo_9to7.pop_front();
    let mut args = Vec::with_capacity(word_count);
    for _ in 0..word_count {
        args.push(shared.ipc.fifo_9to7.pop_front().unwrap_or(0));
    }

    let ok = if args.len() >= 3 {
        let buffer = args[0];
        let first_sector = args[1];
        let num_sectors = args[2];
        match ty {
            BLK_MSG_READ_SECTORS => read_sectors(shared, buffer, first_sector, num_sectors),
            BLK_MSG_WRITE_SECTORS => write_sectors(shared, buffer, first_sector, num_sectors),
            _ => false,
        }
    } else {
        false
    };

    push_reply(shared, u32::from(ok));
    raise_send_empty_if_needed(shared);
    true
}

fn packet_channel(packet: u32) -> u32 {
    packet & 0x1f
}

fn packet_is_response(packet: u32) -> bool {
    packet & PXI_DIR_RESPONSE != 0
}

fn make_reply(imm: u32) -> u32 {
    PXI_CHANNEL_BLKDEV | PXI_DIR_RESPONSE | (imm << 6)
}

fn push_reply(shared: &mut SharedState, imm: u32) {
    let was_empty = shared.ipc.fifo_7to9.is_empty();
    shared.ipc.fifo_7to9.push_back(make_reply(imm));
    if was_empty && shared.ipc.fifo_arm9_recv_irq {
        shared.irq9.request(Irq::IpcRecvNotEmpty);
    }
}

fn raise_send_empty_if_needed(shared: &mut SharedState) {
    if shared.ipc.fifo_9to7.is_empty() && shared.ipc.fifo_arm9_send_empty_irq {
        shared.irq9.request(Irq::IpcSendEmpty);
    }
}

fn ensure_image(shared: &mut SharedState) {
    if shared.dldi_fat_image.is_empty() {
        shared.dldi_fat_image = make_fat16_image();
    }
}

fn write_transfer_sector_count(shared: &mut SharedState) {
    let off = main_ram_offset(TRANSFER_SECTOR_COUNT_ADDR);
    shared.main_ram[off..off + 4].copy_from_slice(&(DLDI_SECTORS as u32).to_le_bytes());
}

fn read_sectors(
    shared: &mut SharedState,
    buffer: u32,
    first_sector: u32,
    num_sectors: u32,
) -> bool {
    ensure_image(shared);
    let Some((src, len)) = sector_range(first_sector, num_sectors) else {
        return false;
    };
    let Some(dst) = main_ram_range(buffer, len) else {
        return false;
    };
    shared.main_ram[dst..dst + len].copy_from_slice(&shared.dldi_fat_image[src..src + len]);
    true
}

fn write_sectors(
    shared: &mut SharedState,
    buffer: u32,
    first_sector: u32,
    num_sectors: u32,
) -> bool {
    ensure_image(shared);
    let Some((dst, len)) = sector_range(first_sector, num_sectors) else {
        return false;
    };
    let Some(src) = main_ram_range(buffer, len) else {
        return false;
    };
    shared.dldi_fat_image[dst..dst + len].copy_from_slice(&shared.main_ram[src..src + len]);
    true
}

fn sector_range(first_sector: u32, num_sectors: u32) -> Option<(usize, usize)> {
    let start = first_sector as usize;
    let count = num_sectors as usize;
    let end = start.checked_add(count)?;
    if end > DLDI_SECTORS {
        return None;
    }
    Some((start * SECTOR_SIZE, count * SECTOR_SIZE))
}

fn main_ram_range(addr: u32, len: usize) -> Option<usize> {
    if addr >> 24 != 0x02 {
        return None;
    }
    let off = main_ram_offset(addr);
    off.checked_add(len)
        .filter(|&end| end <= super::shared::MAIN_RAM_SIZE)?;
    Some(off)
}

fn main_ram_offset(addr: u32) -> usize {
    (addr & 0x003F_FFFF) as usize
}

fn make_fat16_image() -> Vec<u8> {
    const RESERVED: usize = 1;
    const FAT_COUNT: usize = 2;
    const SECTORS_PER_FAT: usize = 128;
    const ROOT_ENTRIES: usize = 512;
    const ROOT_SECTORS: usize = ROOT_ENTRIES * 32 / SECTOR_SIZE;
    const ROOT_START: usize = RESERVED + FAT_COUNT * SECTORS_PER_FAT;
    const DATA_START: usize = ROOT_START + ROOT_SECTORS;
    const README_CLUSTER: u16 = 2;
    const DIR_CLUSTER: u16 = 3;

    let mut img = vec![0u8; DLDI_SECTORS * SECTOR_SIZE];

    {
        let b = &mut img[..SECTOR_SIZE];
        b[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
        b[3..11].copy_from_slice(b"VIBENDS ");
        b[11..13].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
        b[13] = 1;
        b[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
        b[16] = FAT_COUNT as u8;
        b[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
        b[19..21].copy_from_slice(&(DLDI_SECTORS as u16).to_le_bytes());
        b[21] = 0xF8;
        b[22..24].copy_from_slice(&(SECTORS_PER_FAT as u16).to_le_bytes());
        b[24..26].copy_from_slice(&32u16.to_le_bytes());
        b[26..28].copy_from_slice(&64u16.to_le_bytes());
        b[32..36].copy_from_slice(&0u32.to_le_bytes());
        b[36] = 0x80;
        b[38] = 0x29;
        b[39..43].copy_from_slice(&0x564E_4453u32.to_le_bytes());
        b[43..54].copy_from_slice(b"VIBENDS FAT");
        b[54..62].copy_from_slice(b"FAT16   ");
        b[510] = 0x55;
        b[511] = 0xAA;
    }

    for fat in 0..FAT_COUNT {
        let base = (RESERVED + fat * SECTORS_PER_FAT) * SECTOR_SIZE;
        write_u16(&mut img, base, 0xFFF8);
        write_u16(&mut img, base + 2, 0xFFFF);
        write_u16(&mut img, base + README_CLUSTER as usize * 2, 0xFFFF);
        write_u16(&mut img, base + DIR_CLUSTER as usize * 2, 0xFFFF);
    }

    let root = ROOT_START * SECTOR_SIZE;
    write_dir_entry(
        &mut img[root..root + 32],
        b"README  TXT",
        0x20,
        README_CLUSTER,
        b"VibeNDS DLDI test volume\r\n".len() as u32,
    );
    write_dir_entry(
        &mut img[root + 32..root + 64],
        b"GAMES      ",
        0x10,
        DIR_CLUSTER,
        0,
    );

    let readme = (DATA_START + (README_CLUSTER as usize - 2)) * SECTOR_SIZE;
    img[readme..readme + b"VibeNDS DLDI test volume\r\n".len()]
        .copy_from_slice(b"VibeNDS DLDI test volume\r\n");

    let dir = (DATA_START + (DIR_CLUSTER as usize - 2)) * SECTOR_SIZE;
    write_dir_entry(
        &mut img[dir..dir + 32],
        b".          ",
        0x10,
        DIR_CLUSTER,
        0,
    );
    write_dir_entry(&mut img[dir + 32..dir + 64], b"..         ", 0x10, 0, 0);

    img
}

fn write_u16(buf: &mut [u8], off: usize, val: u16) {
    buf[off..off + 2].copy_from_slice(&val.to_le_bytes());
}

fn write_dir_entry(entry: &mut [u8], name: &[u8; 11], attr: u8, first_cluster: u16, size: u32) {
    entry.fill(0);
    entry[0..11].copy_from_slice(name);
    entry[11] = attr;
    entry[26..28].copy_from_slice(&first_cluster.to_le_bytes());
    entry[28..32].copy_from_slice(&size.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::Side;

    fn enable_fifo(shared: &mut SharedState) {
        shared.ipc.write_fifocnt(Side::Arm9, (1 << 15) | (1 << 10));
        shared.ipc.write_fifocnt(Side::Arm7, 1 << 15);
    }

    #[test]
    fn test_blkdev_init_reply_sets_sector_count() {
        let mut shared = SharedState::new();
        enable_fifo(&mut shared);
        shared
            .ipc
            .write_send(Side::Arm9, PXI_CHANNEL_BLKDEV | (BLK_MSG_INIT << 6));

        service_pxi(&mut shared);

        let (reply, _) = shared.ipc.read_recv(Side::Arm9);
        assert_eq!(reply >> 6, 1);
        let off = main_ram_offset(TRANSFER_SECTOR_COUNT_ADDR);
        assert_eq!(
            u32::from_le_bytes(shared.main_ram[off..off + 4].try_into().unwrap()),
            DLDI_SECTORS as u32
        );
    }

    #[test]
    fn test_blkdev_read_boot_sector() {
        let mut shared = SharedState::new();
        enable_fifo(&mut shared);
        let packet = PXI_CHANNEL_EXTENDED
            | (PXI_CHANNEL_BLKDEV << 6)
            | ((3 - 1) << 11)
            | (BLK_MSG_READ_SECTORS << 16);
        shared.ipc.write_send(Side::Arm9, packet);
        shared.ipc.write_send(Side::Arm9, 0x0200_8000);
        shared.ipc.write_send(Side::Arm9, 0);
        shared.ipc.write_send(Side::Arm9, 1);

        service_pxi(&mut shared);

        let (reply, _) = shared.ipc.read_recv(Side::Arm9);
        assert_eq!(reply >> 6, 1);
        let off = main_ram_offset(0x0200_8000);
        assert_eq!(&shared.main_ram[off + 54..off + 62], b"FAT16   ");
        assert_eq!(
            u16::from_le_bytes(shared.main_ram[off + 19..off + 21].try_into().unwrap()),
            DLDI_SECTORS as u16
        );
        assert_eq!(&shared.main_ram[off + 510..off + 512], &[0x55, 0xAA]);
    }
}

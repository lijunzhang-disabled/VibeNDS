//! CP15 — System Control Coprocessor (ARM946E-S only).
//!
//! GBATEK §"ARM9 CP15 Coprocessor" describes the registers we model here.
//! Per the locked architecture decisions, MPU regions are stored but not
//! enforced in Phase 1 (deferred to Phase 9). Cache control ops are NOPs
//! because we don't simulate cache contents. ITCM/DTCM remap is fully
//! functional — it's load-bearing for any ARM9 boot path.

use serde::{Deserialize, Serialize};

/// TCM (Tightly-Coupled Memory) descriptor produced from CP15 c9.
///
/// `size_bytes` is computed from the size field as `512 << size_field`. A
/// `size_bytes` of 0 means the TCM is disabled.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TcmRegion {
    pub base: u32,
    pub size_bytes: u32,
}

impl TcmRegion {
    pub const fn disabled() -> Self {
        TcmRegion {
            base: 0,
            size_bytes: 0,
        }
    }

    /// True when `addr` falls within the TCM window.
    #[inline]
    pub fn contains(&self, addr: u32) -> bool {
        if self.size_bytes == 0 {
            return false;
        }
        let offset = addr.wrapping_sub(self.base);
        offset < self.size_bytes
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct MpuRegion {
    /// Raw `cN, c6, opcode2=N` register value: `[31:12]=base, [5:1]=size_field, [0]=enable`.
    pub raw: u32,
}

impl MpuRegion {
    #[inline]
    pub fn enable(&self) -> bool {
        self.raw & 1 != 0
    }

    #[inline]
    pub fn base(&self) -> u32 {
        self.raw & 0xFFFF_F000
    }

    /// Size field, 0..31. Real region size is `2 ^ (size_field + 1)` bytes.
    #[inline]
    pub fn size_field(&self) -> u32 {
        (self.raw >> 1) & 0x1F
    }
}

/// CP15 register set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cp15 {
    /// c1, c0, opcode2=0: Control Register.
    /// Bits we care about: [13]=high-vector base, [12]=I-cache, [2]=D-cache,
    /// [0]=MPU enable. Other bits are stored verbatim.
    control: u32,

    /// c2, c0: Cacheable region bits (data + instr halves).
    cacheable_data: u32,
    cacheable_instr: u32,

    /// c3, c0: Write-buffer region bits.
    write_buffer: u32,

    /// c5, c0: Access permissions (data + instr halves, ext + std).
    perms_data_std: u32,
    perms_instr_std: u32,
    perms_data_ext: u32,
    perms_instr_ext: u32,

    /// c6: 8 MPU regions.
    pub regions: [MpuRegion; 8],

    /// c9, c1, opcode2=0: D-TCM region register.
    dtcm_raw: u32,

    /// c9, c1, opcode2=1: I-TCM region register.
    itcm_raw: u32,

    /// c13, c0, opcode2=1: context ID / thread-local scratch register.
    /// Modern libnds/calico uses this in its IRQ trampoline to carry the
    /// handled IRQ mask across handler calls before waking scheduler waiters.
    context_id: u32,

    /// Computed TCM views — recomputed whenever `dtcm_raw`/`itcm_raw`/control
    /// change.
    pub itcm: TcmRegion,
    pub dtcm: TcmRegion,
}

impl Cp15 {
    pub fn new() -> Self {
        // NDS ARM9 BIOS-default control: I-cache + D-cache off, high vectors
        // off, MPU off. The BIOS will set bit 13 + caches early in boot.
        let mut cp = Cp15 {
            // ARM946E-S reset state: high vectors off, caches off, MPU off.
            // The NDS ARM9 BIOS flips bit 13 (and bits 12/2 for caches) on boot.
            control: 0x0000_0078,
            cacheable_data: 0,
            cacheable_instr: 0,
            write_buffer: 0,
            perms_data_std: 0,
            perms_instr_std: 0,
            perms_data_ext: 0,
            perms_instr_ext: 0,
            regions: [MpuRegion::default(); 8],
            dtcm_raw: 0,
            itcm_raw: 0,
            context_id: 0,
            itcm: TcmRegion::disabled(),
            dtcm: TcmRegion::disabled(),
        };
        cp.recompute_tcm();
        cp
    }

    /// True when CP15 c1 bit 13 is set — exception vectors live at
    /// `0xFFFF_0000` instead of `0x0000_0000`.
    #[inline]
    pub fn high_vectors(&self) -> bool {
        (self.control >> 13) & 1 != 0
    }

    /// Programmatically set the high-vector bit. Used by `Cpu::new_arm9` to
    /// match the NDS power-on state.
    pub fn set_high_vectors(&mut self, on: bool) {
        if on {
            self.control |= 1 << 13;
        } else {
            self.control &= !(1 << 13);
        }
    }

    #[inline]
    pub fn icache_enabled(&self) -> bool {
        (self.control >> 12) & 1 != 0
    }

    #[inline]
    pub fn dcache_enabled(&self) -> bool {
        (self.control >> 2) & 1 != 0
    }

    #[inline]
    pub fn mpu_enabled(&self) -> bool {
        self.control & 1 != 0
    }

    /// MRC: read CP15 register identified by `(crn, crm, op1, op2)`.
    pub fn read(&self, crn: u32, crm: u32, op1: u32, op2: u32) -> u32 {
        let _ = op1;
        match (crn, crm, op2) {
            (0, 0, 0) => 0x4105_9461, // ARM946E-S Main ID register
            (0, 0, 1) => 0x0F0D_2112, // Cache type: 8KB I-cache, 4KB D-cache, 4-way, 32B lines
            (0, 0, 2) => 0x0014_0180, // TCM size register (ITCM 32K, DTCM 16K reset values)
            (1, 0, 0) => self.control,
            (2, 0, 0) => self.cacheable_data,
            (2, 0, 1) => self.cacheable_instr,
            (3, 0, 0) => self.write_buffer,
            (5, 0, 0) => self.perms_data_std,
            (5, 0, 1) => self.perms_instr_std,
            (5, 0, 2) => self.perms_data_ext,
            (5, 0, 3) => self.perms_instr_ext,
            (6, n @ 0..=7, 0) => self.regions[n as usize].raw,
            (9, 1, 0) => self.dtcm_raw,
            (9, 1, 1) => self.itcm_raw,
            (13, 0, 1) => self.context_id,
            _ => 0,
        }
    }

    /// MCR: write CP15 register identified by `(crn, crm, op1, op2)`.
    pub fn write(&mut self, crn: u32, crm: u32, op1: u32, op2: u32, val: u32) {
        let _ = op1;
        match (crn, crm, op2) {
            (1, 0, 0) => {
                self.control = val;
            }
            (2, 0, 0) => self.cacheable_data = val,
            (2, 0, 1) => self.cacheable_instr = val,
            (3, 0, 0) => self.write_buffer = val,
            (5, 0, 0) => self.perms_data_std = val,
            (5, 0, 1) => self.perms_instr_std = val,
            (5, 0, 2) => self.perms_data_ext = val,
            (5, 0, 3) => self.perms_instr_ext = val,
            (6, n @ 0..=7, 0) => {
                self.regions[n as usize] = MpuRegion { raw: val };
            }
            (7, _, _) => {
                // c7: cache and write-buffer maintenance ops. We don't
                // simulate cache contents, so all of these are NOPs from the
                // emulator's point of view — but writes here are common at
                // boot, so swallow them silently.
            }
            (9, 1, 0) => {
                self.dtcm_raw = val;
                self.recompute_tcm();
            }
            (9, 1, 1) => {
                self.itcm_raw = val;
                self.recompute_tcm();
            }
            (13, 0, 1) => self.context_id = val,
            _ => {
                log::trace!(
                    "CP15 write to unhandled c{},c{},opc2={}: 0x{:08X}",
                    crn,
                    crm,
                    op2,
                    val
                );
            }
        }
    }

    fn recompute_tcm(&mut self) {
        // TCM register format: `[31:12] = base address, [5:1] = size_field`.
        // Existing startup code commonly writes even values such as 0x1E to
        // mirror ITCM across the low address space, so bit 0 is not an enable
        // gate here.
        // Real size = 512 << size_field bytes (size_field=3 → 4 KB, =5 → 32 KB, etc.).
        // ITCM's base is fixed at 0 on this part — the hardware ignores the
        // base bits and mirrors the I-TCM across `[0, itcm.size)`. DTCM's
        // base is honored.
        let dtcm_size = if self.dtcm_raw != 0 {
            let sf = (self.dtcm_raw >> 1) & 0x1F;
            if sf > 31 {
                0
            } else {
                512u32.wrapping_shl(sf)
            }
        } else {
            0
        };
        let dtcm_base = self.dtcm_raw & 0xFFFF_F000;

        let itcm_size = if self.itcm_raw != 0 {
            let sf = (self.itcm_raw >> 1) & 0x1F;
            if sf > 31 {
                0
            } else {
                512u32.wrapping_shl(sf)
            }
        } else {
            0
        };

        self.dtcm = TcmRegion {
            base: dtcm_base,
            size_bytes: dtcm_size,
        };
        self.itcm = TcmRegion {
            base: 0,
            size_bytes: itcm_size,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_high_vector_bit() {
        let mut cp = Cp15::new();
        assert!(!cp.high_vectors());
        cp.write(1, 0, 0, 0, 1 << 13);
        assert!(cp.high_vectors());
    }

    #[test]
    fn test_itcm_remap_to_32k() {
        let mut cp = Cp15::new();
        // size_field = 5 → 512 << 5 = 16 KB (per ARM946 manual: 512 << sf).
        // To get 32 KB we need size_field = 6.
        let raw = (6u32 << 1) | 1;
        cp.write(9, 1, 0, 1, raw); // ITCM
        assert_eq!(cp.itcm.size_bytes, 32 * 1024);
        assert_eq!(cp.itcm.base, 0);
        assert!(cp.itcm.contains(0));
        assert!(cp.itcm.contains(0x7FFF));
        assert!(!cp.itcm.contains(0x8000));
    }

    #[test]
    fn test_itcm_even_raw_value_enables_low_mirror() {
        let mut cp = Cp15::new();
        cp.write(9, 1, 0, 1, 0x1E);
        assert_eq!(cp.itcm.size_bytes, 16 * 1024 * 1024);
        assert!(cp.itcm.contains(0x0080_3EBC));
    }

    #[test]
    fn test_dtcm_remap_to_16k_at_high_base() {
        let mut cp = Cp15::new();
        // size_field = 5 → 16 KB. base = 0x027C0000.
        let raw = 0x027C_0000 | (5u32 << 1) | 1;
        cp.write(9, 1, 0, 0, raw); // DTCM
        assert_eq!(cp.dtcm.size_bytes, 16 * 1024);
        assert_eq!(cp.dtcm.base, 0x027C_0000);
        assert!(cp.dtcm.contains(0x027C_0000));
        assert!(cp.dtcm.contains(0x027C_3FFF));
        assert!(!cp.dtcm.contains(0x027C_4000));
        assert!(!cp.dtcm.contains(0x027B_FFFF));
    }

    #[test]
    fn test_disabled_tcm_contains_nothing() {
        let cp = Cp15::new();
        assert!(!cp.itcm.contains(0));
        assert!(!cp.dtcm.contains(0));
    }

    #[test]
    fn test_mpu_region_storage() {
        let mut cp = Cp15::new();
        // Region 0: base 0x02000000, size_field 0x16 (16 MB), enable.
        let raw = 0x0200_0000 | (0x16 << 1) | 1;
        cp.write(6, 0, 0, 0, raw);
        assert_eq!(cp.regions[0].raw, raw);
        assert!(cp.regions[0].enable());
        assert_eq!(cp.regions[0].base(), 0x0200_0000);
        assert_eq!(cp.regions[0].size_field(), 0x16);
    }

    #[test]
    fn test_main_id_register() {
        let cp = Cp15::new();
        assert_eq!(cp.read(0, 0, 0, 0), 0x4105_9461);
    }

    #[test]
    fn test_context_id_round_trip() {
        let mut cp = Cp15::new();
        cp.write(13, 0, 0, 1, 0x0001_0000);
        assert_eq!(cp.read(13, 0, 0, 1), 0x0001_0000);
    }

    #[test]
    fn test_cache_control_ops_are_nops() {
        let mut cp = Cp15::new();
        // c7,c5,opc2=0 = invalidate I-cache. Should not panic, should not
        // change anything observable.
        cp.write(7, 5, 0, 0, 0);
        cp.write(7, 6, 0, 0, 0);
        cp.write(7, 10, 0, 4, 0);
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MathUnit {
    divcnt: u16,
    div_numer: u64,
    div_denom: u64,
    div_result: u64,
    div_rem: u64,
    sqrtcnt: u16,
    sqrt_param: u64,
    sqrt_result: u32,
}

impl MathUnit {
    pub fn new() -> Self {
        Self {
            divcnt: 0,
            div_numer: 0,
            div_denom: 0,
            div_result: 0,
            div_rem: 0,
            sqrtcnt: 0,
            sqrt_param: 0,
            sqrt_result: 0,
        }
    }

    pub fn read16(&self, off: u32) -> Option<u16> {
        let v = match off {
            0x0280 => self.divcnt & !0x8000,
            0x0290..=0x0296 => half64(self.div_numer, off - 0x0290),
            0x0298..=0x029E => half64(self.div_denom, off - 0x0298),
            0x02A0..=0x02A6 => half64(self.div_result, off - 0x02A0),
            0x02A8..=0x02AE => half64(self.div_rem, off - 0x02A8),
            0x02B0 => self.sqrtcnt & !0x8000,
            0x02B4 | 0x02B6 => half32(self.sqrt_result, off - 0x02B4),
            0x02B8..=0x02BE => half64(self.sqrt_param, off - 0x02B8),
            _ => return None,
        };
        Some(v)
    }

    pub fn read32(&self, off: u32) -> Option<u32> {
        let v = match off {
            0x0280 => self.divcnt as u32,
            0x0290 | 0x0294 => word64(self.div_numer, off - 0x0290),
            0x0298 | 0x029C => word64(self.div_denom, off - 0x0298),
            0x02A0 | 0x02A4 => word64(self.div_result, off - 0x02A0),
            0x02A8 | 0x02AC => word64(self.div_rem, off - 0x02A8),
            0x02B0 => self.sqrtcnt as u32,
            0x02B4 => self.sqrt_result,
            0x02B8 | 0x02BC => word64(self.sqrt_param, off - 0x02B8),
            _ => return None,
        };
        Some(v)
    }

    pub fn write16(&mut self, off: u32, val: u16) -> bool {
        match off {
            0x0280 => {
                self.divcnt = val & 0x0003;
                self.recompute_div();
            }
            0x0290..=0x0296 => {
                set_half64(&mut self.div_numer, off - 0x0290, val);
                self.recompute_div();
            }
            0x0298..=0x029E => {
                set_half64(&mut self.div_denom, off - 0x0298, val);
                self.recompute_div();
            }
            0x02B0 => {
                self.sqrtcnt = val & 0x0001;
                self.recompute_sqrt();
            }
            0x02B8..=0x02BE => {
                set_half64(&mut self.sqrt_param, off - 0x02B8, val);
                self.recompute_sqrt();
            }
            _ => return false,
        }
        true
    }

    pub fn write32(&mut self, off: u32, val: u32) -> bool {
        match off {
            0x0280 => {
                self.divcnt = (val as u16) & 0x0003;
                self.recompute_div();
            }
            0x0290 | 0x0294 => {
                set_word64(&mut self.div_numer, off - 0x0290, val);
                self.recompute_div();
            }
            0x0298 | 0x029C => {
                set_word64(&mut self.div_denom, off - 0x0298, val);
                self.recompute_div();
            }
            0x02B0 => {
                self.sqrtcnt = (val as u16) & 0x0001;
                self.recompute_sqrt();
            }
            0x02B8 | 0x02BC => {
                set_word64(&mut self.sqrt_param, off - 0x02B8, val);
                self.recompute_sqrt();
            }
            _ => return false,
        }
        true
    }

    fn recompute_div(&mut self) {
        let mode = self.divcnt & 0x3;
        let numer = match mode {
            0 => self.div_numer as u32 as i32 as i64,
            _ => self.div_numer as i64,
        };
        let denom = match mode {
            2 => self.div_denom as i64,
            _ => self.div_denom as u32 as i32 as i64,
        };

        if denom == 0 {
            self.div_result = if numer < 0 { 1 } else { u64::MAX };
            self.div_rem = numer as u64;
            self.divcnt = (self.divcnt & !0x4000) | 0x4000;
            return;
        }

        self.div_result = numer.wrapping_div(denom) as u64;
        self.div_rem = numer.wrapping_rem(denom) as u64;
        self.divcnt &= !0x4000;
    }

    fn recompute_sqrt(&mut self) {
        let value = if self.sqrtcnt & 1 == 0 {
            self.sqrt_param as u32 as u64
        } else {
            self.sqrt_param
        };
        self.sqrt_result = isqrt(value);
    }
}

impl Default for MathUnit {
    fn default() -> Self {
        Self::new()
    }
}

fn half32(value: u32, off: u32) -> u16 {
    ((value >> ((off & 2) * 8)) & 0xFFFF) as u16
}

fn half64(value: u64, off: u32) -> u16 {
    ((value >> ((off & 6) * 8)) & 0xFFFF) as u16
}

fn word64(value: u64, off: u32) -> u32 {
    ((value >> ((off & 4) * 8)) & 0xFFFF_FFFF) as u32
}

fn set_half64(value: &mut u64, off: u32, half: u16) {
    let shift = (off & 6) * 8;
    *value = (*value & !(0xFFFFu64 << shift)) | ((half as u64) << shift);
}

fn set_word64(value: &mut u64, off: u32, word: u32) {
    let shift = (off & 4) * 8;
    *value = (*value & !(0xFFFF_FFFFu64 << shift)) | ((word as u64) << shift);
}

fn isqrt(value: u64) -> u32 {
    let mut lo = 0u64;
    let mut hi = 1u64 << 32;
    while lo < hi {
        let mid = (lo + hi + 1) >> 1;
        if mid <= value / mid {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_32_by_32_division() {
        let mut m = MathUnit::new();
        m.write32(0x0290, 7);
        m.write32(0x0298, 2);
        assert_eq!(m.read32(0x02A0), Some(3));
        assert_eq!(m.read32(0x02A8), Some(1));
    }

    #[test]
    fn test_64_by_32_division() {
        let mut m = MathUnit::new();
        m.write16(0x0280, 1);
        m.write32(0x0290, 0);
        m.write32(0x0294, 1);
        m.write32(0x0298, 2);
        assert_eq!(m.read32(0x02A0), Some(0x8000_0000));
        assert_eq!(m.read32(0x02A4), Some(0));
    }

    #[test]
    fn test_signed_division() {
        let mut m = MathUnit::new();
        m.write32(0x0290, (-7i32) as u32);
        m.write32(0x0298, 2);
        assert_eq!(m.read32(0x02A0), Some((-3i32) as u32));
        assert_eq!(m.read32(0x02A8), Some((-1i32) as u32));
    }

    #[test]
    fn test_sqrt_64bit() {
        let mut m = MathUnit::new();
        m.write16(0x02B0, 1);
        m.write32(0x02B8, 0);
        m.write32(0x02BC, 1);
        assert_eq!(m.read32(0x02B4), Some(65536));
    }
}

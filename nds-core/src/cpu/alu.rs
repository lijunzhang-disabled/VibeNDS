//! Barrel shifter and ALU primitives.
//!
//! Identical to the GBA's ARMv4T ALU. Ported verbatim from
//! `../gba/gba-core/src/arm7tdmi/alu.rs`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftType {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
    Ror = 3,
}

impl ShiftType {
    pub fn from_u8(val: u8) -> Self {
        match val & 3 {
            0 => ShiftType::Lsl,
            1 => ShiftType::Lsr,
            2 => ShiftType::Asr,
            3 => ShiftType::Ror,
            _ => unreachable!(),
        }
    }
}

#[inline]
pub fn barrel_shift(value: u32, shift_type: ShiftType, amount: u8, carry_in: bool, immediate: bool) -> (u32, bool) {
    match shift_type {
        ShiftType::Lsl => shift_lsl(value, amount, carry_in),
        ShiftType::Lsr => shift_lsr(value, amount, carry_in, immediate),
        ShiftType::Asr => shift_asr(value, amount, carry_in, immediate),
        ShiftType::Ror => shift_ror(value, amount, carry_in, immediate),
    }
}

fn shift_lsl(value: u32, amount: u8, carry_in: bool) -> (u32, bool) {
    match amount {
        0 => (value, carry_in),
        1..=31 => {
            let carry = (value >> (32 - amount)) & 1 != 0;
            (value << amount, carry)
        }
        32 => (0, value & 1 != 0),
        _ => (0, false),
    }
}

fn shift_lsr(value: u32, amount: u8, carry_in: bool, immediate: bool) -> (u32, bool) {
    match amount {
        0 => {
            if immediate {
                (0, value >> 31 != 0)
            } else {
                (value, carry_in)
            }
        }
        1..=31 => {
            let carry = (value >> (amount - 1)) & 1 != 0;
            (value >> amount, carry)
        }
        32 => (0, value >> 31 != 0),
        _ => (0, false),
    }
}

fn shift_asr(value: u32, amount: u8, carry_in: bool, immediate: bool) -> (u32, bool) {
    match amount {
        0 => {
            if immediate {
                let carry = (value as i32) < 0;
                let result = if carry { 0xFFFF_FFFF } else { 0 };
                (result, carry)
            } else {
                (value, carry_in)
            }
        }
        1..=31 => {
            let carry = ((value as i32) >> (amount - 1)) & 1 != 0;
            ((value as i32 >> amount) as u32, carry)
        }
        _ => {
            let carry = (value as i32) < 0;
            let result = if carry { 0xFFFF_FFFF } else { 0 };
            (result, carry)
        }
    }
}

fn shift_ror(value: u32, amount: u8, carry_in: bool, immediate: bool) -> (u32, bool) {
    match amount {
        0 => {
            if immediate {
                // RRX
                let result = (carry_in as u32) << 31 | (value >> 1);
                let carry = value & 1 != 0;
                (result, carry)
            } else {
                (value, carry_in)
            }
        }
        _ => {
            let amount = amount & 31;
            if amount == 0 {
                (value, value >> 31 != 0)
            } else {
                let result = value.rotate_right(amount as u32);
                let carry = result >> 31 != 0;
                (result, carry)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluOp {
    And = 0x0, Eor = 0x1, Sub = 0x2, Rsb = 0x3,
    Add = 0x4, Adc = 0x5, Sbc = 0x6, Rsc = 0x7,
    Tst = 0x8, Teq = 0x9, Cmp = 0xA, Cmn = 0xB,
    Orr = 0xC, Mov = 0xD, Bic = 0xE, Mvn = 0xF,
}

impl AluOp {
    pub fn from_u8(val: u8) -> Self {
        match val & 0xF {
            0x0 => AluOp::And, 0x1 => AluOp::Eor, 0x2 => AluOp::Sub, 0x3 => AluOp::Rsb,
            0x4 => AluOp::Add, 0x5 => AluOp::Adc, 0x6 => AluOp::Sbc, 0x7 => AluOp::Rsc,
            0x8 => AluOp::Tst, 0x9 => AluOp::Teq, 0xA => AluOp::Cmp, 0xB => AluOp::Cmn,
            0xC => AluOp::Orr, 0xD => AluOp::Mov, 0xE => AluOp::Bic, 0xF => AluOp::Mvn,
            _ => unreachable!(),
        }
    }

    pub fn is_test(self) -> bool {
        matches!(self, AluOp::Tst | AluOp::Teq | AluOp::Cmp | AluOp::Cmn)
    }

    pub fn is_logical(self) -> bool {
        matches!(
            self,
            AluOp::And | AluOp::Eor | AluOp::Tst | AluOp::Teq
                | AluOp::Orr | AluOp::Mov | AluOp::Bic | AluOp::Mvn
        )
    }
}

#[inline]
pub fn add_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    let result = (a as u64) + (b as u64) + (carry_in as u64);
    let result32 = result as u32;
    let carry = result > 0xFFFF_FFFF;
    let overflow = ((a ^ result32) & (b ^ result32)) >> 31 != 0;
    (result32, carry, overflow)
}

#[inline]
pub fn sub_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    add_with_carry(a, !b, carry_in)
}

/// Saturating signed add — clamps to i32 range. Used by ARMv5TE QADD.
#[inline]
pub fn signed_sat_add(a: i32, b: i32) -> (i32, bool) {
    let (sum, overflow) = a.overflowing_add(b);
    if overflow {
        (if a < 0 { i32::MIN } else { i32::MAX }, true)
    } else {
        (sum, false)
    }
}

/// Saturating signed sub — clamps to i32 range. Used by ARMv5TE QSUB.
#[inline]
pub fn signed_sat_sub(a: i32, b: i32) -> (i32, bool) {
    let (diff, overflow) = a.overflowing_sub(b);
    if overflow {
        (if a < 0 { i32::MIN } else { i32::MAX }, true)
    } else {
        (diff, false)
    }
}

/// Saturating signed double — clamps to i32 range. Used by ARMv5TE QDADD/QDSUB.
#[inline]
pub fn signed_sat_double(a: i32) -> (i32, bool) {
    signed_sat_add(a, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lsl() {
        assert_eq!(barrel_shift(0x80000000, ShiftType::Lsl, 1, false, false), (0, true));
        assert_eq!(barrel_shift(1, ShiftType::Lsl, 31, false, false), (0x80000000, false));
        assert_eq!(barrel_shift(0xFF, ShiftType::Lsl, 0, true, false), (0xFF, true));
        assert_eq!(barrel_shift(0xFF, ShiftType::Lsl, 0, false, false), (0xFF, false));
    }

    #[test]
    fn test_lsr() {
        assert_eq!(barrel_shift(1, ShiftType::Lsr, 1, false, false), (0, true));
        assert_eq!(barrel_shift(0x80000000, ShiftType::Lsr, 31, false, false), (1, false));
        assert_eq!(barrel_shift(0x80000000, ShiftType::Lsr, 0, false, true), (0, true));
    }

    #[test]
    fn test_asr() {
        assert_eq!(barrel_shift(0x80000000, ShiftType::Asr, 1, false, false), (0xC0000000, false));
        assert_eq!(barrel_shift(0x80000000, ShiftType::Asr, 31, false, false), (0xFFFFFFFF, false));
        assert_eq!(barrel_shift(0x80000000, ShiftType::Asr, 0, false, true), (0xFFFFFFFF, true));
        assert_eq!(barrel_shift(0x7FFFFFFF, ShiftType::Asr, 0, false, true), (0, false));
    }

    #[test]
    fn test_ror() {
        assert_eq!(barrel_shift(1, ShiftType::Ror, 1, false, false), (0x80000000, true));
        assert_eq!(barrel_shift(0x80000000, ShiftType::Ror, 1, false, false), (0x40000000, false));
    }

    #[test]
    fn test_rrx() {
        assert_eq!(barrel_shift(1, ShiftType::Ror, 0, true, true), (0x80000000, true));
        assert_eq!(barrel_shift(1, ShiftType::Ror, 0, false, true), (0, true));
        assert_eq!(barrel_shift(0, ShiftType::Ror, 0, true, true), (0x80000000, false));
    }

    #[test]
    fn test_add_with_carry() {
        assert_eq!(add_with_carry(0xFFFFFFFF, 1, false), (0, true, false));
        assert_eq!(add_with_carry(0x7FFFFFFF, 1, false), (0x80000000, false, true));
        assert_eq!(add_with_carry(0x80000000, 0x80000000, false), (0, true, true));
    }

    #[test]
    fn test_sub_with_carry() {
        assert_eq!(sub_with_carry(5, 3, true), (2, true, false));
        assert_eq!(sub_with_carry(3, 5, true), (0xFFFFFFFE, false, false));
        assert_eq!(sub_with_carry(0, 1, true), (0xFFFFFFFF, false, false));
    }

    #[test]
    fn test_signed_sat_add() {
        assert_eq!(signed_sat_add(1, 2), (3, false));
        assert_eq!(signed_sat_add(i32::MAX, 1), (i32::MAX, true));
        assert_eq!(signed_sat_add(i32::MIN, -1), (i32::MIN, true));
    }

    #[test]
    fn test_signed_sat_sub() {
        assert_eq!(signed_sat_sub(5, 3), (2, false));
        assert_eq!(signed_sat_sub(i32::MIN, 1), (i32::MIN, true));
        assert_eq!(signed_sat_sub(i32::MAX, -1), (i32::MAX, true));
    }

    #[test]
    fn test_signed_sat_double() {
        assert_eq!(signed_sat_double(0x3FFF_FFFF), (0x7FFF_FFFE, false));
        assert_eq!(signed_sat_double(0x4000_0000), (i32::MAX, true));
        assert_eq!(signed_sat_double(-0x4000_0001), (i32::MIN, true));
    }
}

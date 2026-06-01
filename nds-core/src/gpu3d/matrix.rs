//! 4×4 matrix in 1.19.12 fixed-point.
//!
//! Storage is **row-major** to match the NDS GPU command parameter order:
//!
//! ```text
//! storage index:    0   1   2   3      ← row 0
//!                   4   5   6   7      ← row 1
//!                   8   9  10  11      ← row 2
//!                  12  13  14  15      ← row 3
//!
//! mathematical layout:  M[row, col]
//!     [ M[0]  M[1]  M[2]  M[3]  ]   row 0
//!     [ M[4]  M[5]  M[6]  M[7]  ]   row 1
//!     [ M[8]  M[9]  M[10] M[11] ]   row 2
//!     [ M[12] M[13] M[14] M[15] ]   row 3
//! ```
//!
//! The NDS GX command set sends matrix data as 16 consecutive 32-bit
//! parameter words in this same row-major order, so `Matrix::load_4x4`
//! just copies the slice.
//!
//! All entries are 1.19.12 signed fixed-point (`i32`). Multiplication
//! uses `i64` intermediates and shifts the 24-fractional-bit product back
//! down by 12 to keep the result in 1.19.12.

use serde::{Deserialize, Serialize};

/// One unit in 1.19.12 fixed-point.
pub const ONE: i32 = 1 << 12;

/// Multiply two 1.19.12 fixed-point values, returning the 1.19.12 result.
/// Uses an `i64` intermediate to avoid overflow on the 24-bit product.
#[inline]
pub fn fmul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 12) as i32
}

/// 4×4 row-major matrix of 1.19.12 fixed-point values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Matrix {
    pub m: [i32; 16],
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix {
        m: [ONE, 0, 0, 0, 0, ONE, 0, 0, 0, 0, ONE, 0, 0, 0, 0, ONE],
    };

    pub fn identity() -> Self {
        Self::IDENTITY
    }

    /// Get the element at `[row, col]`.
    #[inline]
    pub fn at(&self, row: usize, col: usize) -> i32 {
        self.m[row * 4 + col]
    }

    #[inline]
    fn at_mut(&mut self, row: usize, col: usize) -> &mut i32 {
        &mut self.m[row * 4 + col]
    }

    /// Load all 16 entries from a row-major slice (NDS `MTX_LOAD_4x4`).
    pub fn load_4x4(words: &[i32; 16]) -> Self {
        Matrix { m: *words }
    }

    /// Load a 4×3 matrix (12 words). The bottom row becomes `[0, 0, 0, 1]`.
    /// This is the NDS `MTX_LOAD_4x3` and `MTX_MULT_4x3` parameter shape.
    pub fn load_4x3(words: &[i32; 12]) -> Self {
        let mut m = [0i32; 16];
        // Source is row-major 4×3:
        // [m0 m1 m2 0; m3 m4 m5 0; m6 m7 m8 0; m9 m10 m11 1].
        for row in 0..4 {
            for col in 0..3 {
                m[row * 4 + col] = words[row * 3 + col];
            }
        }
        // Last column = [0, 0, 0, 1] for affine matrices.
        m[3] = 0;
        m[7] = 0;
        m[11] = 0;
        m[15] = ONE;
        Matrix { m }
    }

    /// Load a 3×3 matrix (9 words). Used by `MTX_LOAD_3x3` for rotation-only
    /// transforms (e.g. lighting normal matrices). Extends to 4×4 with the
    /// identity in row 3 and column 3.
    pub fn load_3x3(words: &[i32; 9]) -> Self {
        let mut m = Self::IDENTITY.m;
        for row in 0..3 {
            for col in 0..3 {
                m[row * 4 + col] = words[row * 3 + col];
            }
        }
        Matrix { m }
    }

    /// Matrix × matrix. `self × other` returns a fresh matrix; the command
    /// layer decides which operand is the current matrix and which is the
    /// incoming command matrix.
    pub fn mul_matrix(&self, other: &Matrix) -> Matrix {
        let mut out = [0i32; 16];
        for col in 0..4 {
            for row in 0..4 {
                let mut acc: i64 = 0;
                for k in 0..4 {
                    acc += (self.at(row, k) as i64) * (other.at(k, col) as i64);
                }
                out[row * 4 + col] = (acc >> 12) as i32;
            }
        }
        Matrix { m: out }
    }

    /// Multiply a row-vector `(x, y, z, w)` by the matrix. Returns the
    /// transformed `(x', y', z', w')`.
    pub fn mul_vec4(&self, v: [i32; 4]) -> [i32; 4] {
        let mut out = [0i32; 4];
        for col in 0..4 {
            let mut acc: i64 = 0;
            for row in 0..4 {
                acc += (v[row] as i64) * (self.at(row, col) as i64);
            }
            out[col] = (acc >> 12) as i32;
        }
        out
    }

    /// Return `self × T(tx, ty, tz)`.
    pub fn mul_translate(&self, tx: i32, ty: i32, tz: i32) -> Matrix {
        let t = Matrix {
            m: [ONE, 0, 0, 0, 0, ONE, 0, 0, 0, 0, ONE, 0, tx, ty, tz, ONE],
        };
        self.mul_matrix(&t)
    }

    /// Return `self × S(sx, sy, sz)`.
    pub fn mul_scale(&self, sx: i32, sy: i32, sz: i32) -> Matrix {
        let s = Matrix {
            m: [sx, 0, 0, 0, 0, sy, 0, 0, 0, 0, sz, 0, 0, 0, 0, ONE],
        };
        self.mul_matrix(&s)
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: i32, b: i32, tol: i32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn test_identity_is_neutral_for_multiply() {
        let m = Matrix::identity();
        let n = m.mul_matrix(&Matrix::identity());
        assert_eq!(m, n);
    }

    #[test]
    fn test_identity_passes_through_vector() {
        let m = Matrix::identity();
        let v = [ONE * 3, ONE * 5, ONE * -7, ONE];
        let r = m.mul_vec4(v);
        assert_eq!(r, v);
    }

    #[test]
    fn test_fmul_basic() {
        // 2.0 × 3.0 = 6.0
        assert_eq!(fmul(2 * ONE, 3 * ONE), 6 * ONE);
        // 0.5 × 0.5 = 0.25
        assert_eq!(fmul(ONE / 2, ONE / 2), ONE / 4);
        // -1.0 × 2.0 = -2.0
        assert_eq!(fmul(-ONE, 2 * ONE), -2 * ONE);
    }

    #[test]
    fn test_translate_moves_origin() {
        // T(10, 20, 30) applied to the origin gives (10, 20, 30).
        let m = Matrix::identity().mul_translate(10 * ONE, 20 * ONE, 30 * ONE);
        let r = m.mul_vec4([0, 0, 0, ONE]);
        assert_eq!(r, [10 * ONE, 20 * ONE, 30 * ONE, ONE]);
    }

    #[test]
    fn test_scale_doubles_vector() {
        let m = Matrix::identity().mul_scale(2 * ONE, 2 * ONE, 2 * ONE);
        let r = m.mul_vec4([ONE, 2 * ONE, 3 * ONE, ONE]);
        assert_eq!(r, [2 * ONE, 4 * ONE, 6 * ONE, ONE]);
    }

    #[test]
    fn test_load_4x4_round_trip() {
        let words: [i32; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let m = Matrix::load_4x4(&words);
        // Row 0 is M[0..3].
        assert_eq!(m.at(0, 0), 1);
        assert_eq!(m.at(0, 1), 2);
        assert_eq!(m.at(0, 2), 3);
        assert_eq!(m.at(0, 3), 4);
        // Row 3 is M[12..15].
        assert_eq!(m.at(3, 0), 13);
        assert_eq!(m.at(3, 3), 16);
    }

    #[test]
    fn test_load_4x3_pads_bottom_row() {
        let words: [i32; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let m = Matrix::load_4x3(&words);
        assert_eq!(m.at(3, 0), 10);
        assert_eq!(m.at(3, 1), 11);
        assert_eq!(m.at(3, 2), 12);
        assert_eq!(m.at(3, 3), ONE);
        assert_eq!(m.at(0, 3), 0);
        assert_eq!(m.at(1, 3), 0);
        assert_eq!(m.at(2, 3), 0);
        // Top-left should be 1
        assert_eq!(m.at(0, 0), 1);
    }

    #[test]
    fn test_load_3x3_pads_identity() {
        let words: [i32; 9] = [2 * ONE, 0, 0, 0, 2 * ONE, 0, 0, 0, 2 * ONE];
        let m = Matrix::load_3x3(&words);
        // Last column/row should remain identity.
        assert_eq!(m.at(0, 3), 0);
        assert_eq!(m.at(3, 3), ONE);
        // 2× scale on the 3×3 portion.
        assert_eq!(m.at(0, 0), 2 * ONE);
        let r = m.mul_vec4([ONE, ONE, ONE, ONE]);
        assert_eq!(r, [2 * ONE, 2 * ONE, 2 * ONE, ONE]);
    }

    #[test]
    fn test_mul_matrix_associative_under_translate_compose() {
        // T(1, 0, 0) × T(0, 2, 0) = T(1, 2, 0). Apply to origin → (1, 2, 0).
        let a = Matrix::identity().mul_translate(ONE, 0, 0);
        let b = Matrix::identity().mul_translate(0, 2 * ONE, 0);
        let c = a.mul_matrix(&b);
        let r = c.mul_vec4([0, 0, 0, ONE]);
        assert_eq!(r, [ONE, 2 * ONE, 0, ONE]);
    }

    #[test]
    fn test_fixed_point_precision_quarter() {
        // 0.25 × 4.0 = 1.0 with no precision loss in 1.19.12.
        assert_eq!(fmul(ONE / 4, 4 * ONE), ONE);
    }

    #[test]
    fn test_mul_matrix_rotates_then_scales() {
        // Verify ordering: row-vector v × M means "apply M to v in object space".
        let s = Matrix::identity().mul_scale(2 * ONE, 2 * ONE, 2 * ONE);
        let c = s.mul_matrix(&Matrix::identity().mul_translate(5 * ONE, 0, 0));
        let r = c.mul_vec4([ONE, 0, 0, ONE]);
        assert_eq!(r[0], 7 * ONE);
        let _ = approx_eq;
    }
}

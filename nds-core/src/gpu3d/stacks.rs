//! Matrix stacks driven by `MTX_MODE` + `MTX_*` GX commands.
//!
//! NDS has four matrix stacks selected by `MTX_MODE` (`0x10`):
//!
//! | Mode | Stack | Depth | Notes |
//! |---|---|---:|---|
//! | 0 | Projection | 1 | A single "previous" slot; push/pop bookkeeping only. |
//! | 1 | Position | 32 | Combined model×view. Mode 2 also updates this stack. |
//! | 2 | Position + Vector | 32 | The position matrix *and* a matching "direction-only" matrix (used for normal-vector lighting); they're kept lockstep. |
//! | 3 | Texture | 1 | Transforms per-vertex UVs. |
//!
//! Per-mode `MTX_PUSH` / `MTX_POP` / `MTX_STORE` / `MTX_RESTORE` operate on
//! these stacks. Overflow / underflow sets the corresponding bit in
//! `GXSTAT` (we track them on the stack so the caller can read them).
//!
//! Reference: GBATEK §"DS 3D Geometry Commands — Matrices".

use serde::{Deserialize, Serialize};

use super::matrix::Matrix;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MtxMode { Projection = 0, Position = 1, PosVector = 2, Texture = 3 }

impl MtxMode {
    pub fn from_bits(bits: u32) -> Self {
        match bits & 0x3 {
            0 => MtxMode::Projection,
            1 => MtxMode::Position,
            2 => MtxMode::PosVector,
            _ => MtxMode::Texture,
        }
    }
}

/// All four matrix stacks + the current matrix for each.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixStacks {
    pub mode: MtxMode,

    /// Currently-active matrices (modified by `MTX_LOAD_*`, `MTX_MULT_*`, etc.)
    pub projection: Matrix,
    pub position: Matrix,
    pub vector: Matrix,
    pub texture: Matrix,

    /// Saved snapshot for projection (1-deep).
    pub projection_saved: Matrix,
    pub projection_saved_valid: bool,

    /// 32-deep stack for the position/vector pair (kept lockstep).
    pub position_stack: Vec<Matrix>,
    pub vector_stack: Vec<Matrix>,

    /// Stack pointer for the position pair. Increments on PUSH, decrements
    /// on POP / explicit RESTORE.
    pub position_sp: u8,

    /// Saved snapshot for texture (1-deep).
    pub texture_saved: Matrix,
    pub texture_saved_valid: bool,

    /// Sticky overflow flag (`GXSTAT` bit 15). Set on push past the end of
    /// the 32-deep position stack, or pop on an empty stack, or any depth
    /// argument out of range.
    pub overflow: bool,
}

impl MatrixStacks {
    pub fn new() -> Self {
        MatrixStacks {
            mode: MtxMode::Projection,
            projection: Matrix::identity(),
            position: Matrix::identity(),
            vector: Matrix::identity(),
            texture: Matrix::identity(),
            projection_saved: Matrix::identity(),
            projection_saved_valid: false,
            position_stack: vec![Matrix::identity(); 32],
            vector_stack: vec![Matrix::identity(); 32],
            position_sp: 0,
            texture_saved: Matrix::identity(),
            texture_saved_valid: false,
            overflow: false,
        }
    }

    /// `MTX_MODE` — select the active stack.
    pub fn set_mode(&mut self, mode: MtxMode) {
        self.mode = mode;
    }

    /// `MTX_IDENTITY` — clear the current matrix to identity.
    pub fn identity(&mut self) {
        match self.mode {
            MtxMode::Projection => self.projection = Matrix::identity(),
            MtxMode::Position => self.position = Matrix::identity(),
            MtxMode::PosVector => {
                self.position = Matrix::identity();
                self.vector = Matrix::identity();
            }
            MtxMode::Texture => self.texture = Matrix::identity(),
        }
    }

    /// `MTX_LOAD_4x4` — replace the current matrix.
    pub fn load(&mut self, m: Matrix) {
        match self.mode {
            MtxMode::Projection => self.projection = m,
            MtxMode::Position => self.position = m,
            MtxMode::PosVector => {
                self.position = m;
                self.vector = m;
            }
            MtxMode::Texture => self.texture = m,
        }
    }

    /// `MTX_MULT_4x4` (etc.) — post-multiply the current matrix by `m`.
    /// In mode 2 both `position` and `vector` get post-multiplied (kept
    /// lockstep, per GBATEK).
    pub fn mult(&mut self, m: Matrix) {
        match self.mode {
            MtxMode::Projection => self.projection = self.projection.mul_matrix(&m),
            MtxMode::Position => self.position = self.position.mul_matrix(&m),
            MtxMode::PosVector => {
                self.position = self.position.mul_matrix(&m);
                self.vector = self.vector.mul_matrix(&m);
            }
            MtxMode::Texture => self.texture = self.texture.mul_matrix(&m),
        }
    }

    /// `MTX_PUSH` — save current onto its stack.
    pub fn push(&mut self) {
        match self.mode {
            MtxMode::Projection | MtxMode::Texture => {
                let (saved, valid) = match self.mode {
                    MtxMode::Projection => (&mut self.projection_saved, &mut self.projection_saved_valid),
                    MtxMode::Texture    => (&mut self.texture_saved,    &mut self.texture_saved_valid),
                    _ => unreachable!(),
                };
                if *valid {
                    self.overflow = true; // already-saved → overwrite-with-overflow
                }
                *saved = match self.mode {
                    MtxMode::Projection => self.projection,
                    MtxMode::Texture    => self.texture,
                    _ => unreachable!(),
                };
                *valid = true;
            }
            MtxMode::Position | MtxMode::PosVector => {
                if self.position_sp >= 31 {
                    self.overflow = true;
                    return;
                }
                self.position_stack[self.position_sp as usize] = self.position;
                self.vector_stack[self.position_sp as usize] = self.vector;
                self.position_sp += 1;
            }
        }
    }

    /// `MTX_POP` — restore current from its stack. The parameter is a
    /// signed 6-bit offset: 1..31 = "pop n levels", 32..63 = "pop -n
    /// levels" (i.e. push back up by N — used by some games to wind the
    /// stack pointer back after deep traversals).
    pub fn pop(&mut self, count_param: u32) {
        // Interpret as signed 6-bit (range -32..31). Per GBATEK only the
        // sign matters for the underflow direction.
        let signed_count = (((count_param & 0x3F) as i32) << 26) >> 26;
        match self.mode {
            MtxMode::Projection | MtxMode::Texture => {
                let valid = match self.mode {
                    MtxMode::Projection => &mut self.projection_saved_valid,
                    MtxMode::Texture    => &mut self.texture_saved_valid,
                    _ => unreachable!(),
                };
                if !*valid {
                    self.overflow = true;
                    return;
                }
                match self.mode {
                    MtxMode::Projection => self.projection = self.projection_saved,
                    MtxMode::Texture    => self.texture = self.texture_saved,
                    _ => unreachable!(),
                }
                *valid = false;
            }
            MtxMode::Position | MtxMode::PosVector => {
                let new_sp = (self.position_sp as i32) - signed_count;
                if new_sp < 0 || new_sp >= 32 {
                    self.overflow = true;
                    let clamped = new_sp.clamp(0, 31) as u8;
                    self.position_sp = clamped;
                } else {
                    self.position_sp = new_sp as u8;
                }
                let sp = self.position_sp as usize;
                self.position = self.position_stack[sp];
                self.vector = self.vector_stack[sp];
            }
        }
    }

    /// `MTX_STORE` — copy current to a specific stack slot (0..30 for the
    /// position pair). For projection/texture this acts like push (single
    /// slot).
    pub fn store(&mut self, slot: u32) {
        match self.mode {
            MtxMode::Projection => {
                self.projection_saved = self.projection;
                self.projection_saved_valid = true;
            }
            MtxMode::Texture => {
                self.texture_saved = self.texture;
                self.texture_saved_valid = true;
            }
            MtxMode::Position | MtxMode::PosVector => {
                let idx = (slot & 0x1F) as usize;
                if idx >= 31 {
                    self.overflow = true;
                    return;
                }
                self.position_stack[idx] = self.position;
                self.vector_stack[idx] = self.vector;
            }
        }
    }

    /// `MTX_RESTORE` — load current from a specific stack slot.
    pub fn restore(&mut self, slot: u32) {
        match self.mode {
            MtxMode::Projection => {
                if self.projection_saved_valid {
                    self.projection = self.projection_saved;
                } else {
                    self.overflow = true;
                }
            }
            MtxMode::Texture => {
                if self.texture_saved_valid {
                    self.texture = self.texture_saved;
                } else {
                    self.overflow = true;
                }
            }
            MtxMode::Position | MtxMode::PosVector => {
                let idx = (slot & 0x1F) as usize;
                if idx >= 31 {
                    self.overflow = true;
                    return;
                }
                self.position = self.position_stack[idx];
                self.vector = self.vector_stack[idx];
            }
        }
    }

    /// Read-only access to whatever the current matrix is for this mode.
    pub fn current(&self) -> &Matrix {
        match self.mode {
            MtxMode::Projection => &self.projection,
            MtxMode::Position | MtxMode::PosVector => &self.position,
            MtxMode::Texture => &self.texture,
        }
    }

    /// `clip = position × projection`. Used per vertex during the geometry
    /// pipeline. We cache neither — recomputed on every `VTX_*` because
    /// matrices change often relative to vertices per typical game patterns.
    pub fn clip_matrix(&self) -> Matrix {
        self.position.mul_matrix(&self.projection)
    }
}

impl Default for MatrixStacks {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::matrix::ONE;

    #[test]
    fn test_load_and_mult_in_projection_mode() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Projection);
        let m = Matrix::identity().mul_translate(3 * ONE, 0, 0);
        s.load(m);
        assert_eq!(s.projection.at(3, 0), 3 * ONE);
        // mult by T(2,0,0) → total translate (5,0,0)
        s.mult(Matrix::identity().mul_translate(2 * ONE, 0, 0));
        let r = s.projection.mul_vec4([0, 0, 0, ONE]);
        assert_eq!(r[0], 5 * ONE);
    }

    #[test]
    fn test_pos_vector_mode_keeps_lockstep() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::PosVector);
        s.mult(Matrix::identity().mul_translate(7 * ONE, 0, 0));
        // Both position and vector got the multiply applied.
        assert_eq!(s.position.at(3, 0), 7 * ONE);
        assert_eq!(s.vector.at(3, 0), 7 * ONE);
    }

    #[test]
    fn test_position_push_pop_balance() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Position);
        s.mult(Matrix::identity().mul_translate(ONE, 0, 0));
        s.push();
        assert_eq!(s.position_sp, 1);
        // Apply another transform on top.
        s.mult(Matrix::identity().mul_translate(2 * ONE, 0, 0));
        // Pop returns to the post-push state.
        s.pop(1);
        assert_eq!(s.position_sp, 0);
        let r = s.position.mul_vec4([0, 0, 0, ONE]);
        assert_eq!(r[0], ONE, "pop should restore T(1, 0, 0)");
    }

    #[test]
    fn test_projection_push_overflow_sets_flag() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Projection);
        s.push();
        assert!(!s.overflow);
        // Second push overwrites the single saved slot — sets overflow.
        s.push();
        assert!(s.overflow);
    }

    #[test]
    fn test_position_push_overflow_at_depth_31() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Position);
        for _ in 0..31 { s.push(); }
        assert_eq!(s.position_sp, 31);
        // 32nd push would go past the last valid slot.
        s.push();
        assert!(s.overflow);
        assert_eq!(s.position_sp, 31); // pointer didn't advance
    }

    #[test]
    fn test_pop_signed_offset() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Position);
        for _ in 0..5 { s.push(); }
        assert_eq!(s.position_sp, 5);
        // Pop 2 levels: stack pointer goes from 5 → 3.
        s.pop(2);
        assert_eq!(s.position_sp, 3);
        // Negative pop count = push back up: 3 - (-2) = 5
        s.pop(0x3E); // 0x3E sign-extended in 6 bits = -2
        assert_eq!(s.position_sp, 5);
    }

    #[test]
    fn test_store_restore_position_slot() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Position);
        let target = Matrix::identity().mul_translate(9 * ONE, 0, 0);
        s.load(target);
        s.store(5);
        // Wipe current.
        s.identity();
        let r = s.position.mul_vec4([0, 0, 0, ONE]);
        assert_eq!(r[0], 0);
        // Restore from slot 5 — should bring back the translate.
        s.restore(5);
        let r = s.position.mul_vec4([0, 0, 0, ONE]);
        assert_eq!(r[0], 9 * ONE);
    }

    #[test]
    fn test_clip_matrix_is_pos_times_proj() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Projection);
        s.load(Matrix::identity().mul_scale(2 * ONE, 2 * ONE, 2 * ONE));
        s.set_mode(MtxMode::Position);
        s.load(Matrix::identity().mul_translate(ONE, 0, 0));
        // Row-vector clip = pos × proj applied to origin: translate first,
        // then scale by 2.
        let clip = s.clip_matrix();
        let r = clip.mul_vec4([0, 0, 0, ONE]);
        assert_eq!(r[0], 2 * ONE);
    }

    #[test]
    fn test_pop_empty_projection_sets_overflow() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Projection);
        s.pop(1);
        assert!(s.overflow);
    }
}

//! Matrix stacks driven by `MTX_MODE` + `MTX_*` GX commands.
//!
//! NDS has four matrix stacks selected by `MTX_MODE` (`0x10`):
//!
//! | Mode | Stack | Depth | Notes |
//! |---|---|---:|---|
//! | 0 | Projection | 1 | A single "previous" slot; push/pop bookkeeping only. |
//! | 1 | Position | 32 | Combined model×view. Stack ops also touch Vector. |
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
pub enum MtxMode {
    Projection = 0,
    Position = 1,
    PosVector = 2,
    Texture = 3,
}

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

    /// Saved snapshot for projection (1-deep) and its 1-bit stack pointer.
    /// `MTX_STORE`/`MTX_RESTORE` access the saved slot without moving the
    /// pointer; only `MTX_PUSH`/`MTX_POP` change `projection_sp`.
    pub projection_saved: Matrix,
    pub projection_sp: u8,

    /// 32 mirrored entries for the position/vector pair (kept lockstep).
    pub position_stack: Vec<Matrix>,
    pub vector_stack: Vec<Matrix>,

    /// 6-bit stack pointer for the position pair. GXSTAT exposes the lower
    /// five bits; entries 32..63 mirror 0..31 and set the overflow flag.
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
            projection_sp: 0,
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

    /// `MTX_MULT_4x4` (etc.) — hardware applies the command matrix before
    /// the current matrix: `current = m * current`.
    pub fn mult(&mut self, m: Matrix) {
        match self.mode {
            MtxMode::Projection => self.projection = m.mul_matrix(&self.projection),
            MtxMode::Position => self.position = m.mul_matrix(&self.position),
            MtxMode::PosVector => {
                self.position = m.mul_matrix(&self.position);
                self.vector = m.mul_matrix(&self.vector);
            }
            MtxMode::Texture => self.texture = m.mul_matrix(&self.texture),
        }
    }

    /// `MTX_SCALE` — pre-multiply by a scale matrix, except mode 2 updates
    /// only the position matrix so the directional matrix keeps light-vector
    /// lengths.
    pub fn scale(&mut self, sx: i32, sy: i32, sz: i32) {
        let m = Matrix::identity().mul_scale(sx, sy, sz);
        match self.mode {
            MtxMode::Projection => self.projection = m.mul_matrix(&self.projection),
            MtxMode::Position | MtxMode::PosVector => {
                self.position = m.mul_matrix(&self.position);
            }
            MtxMode::Texture => self.texture = m.mul_matrix(&self.texture),
        }
    }

    /// `MTX_TRANS` — pre-multiply the selected matrix/matrices by a
    /// translation matrix.
    pub fn translate(&mut self, tx: i32, ty: i32, tz: i32) {
        let m = Matrix::identity().mul_translate(tx, ty, tz);
        match self.mode {
            MtxMode::Projection => self.projection = m.mul_matrix(&self.projection),
            MtxMode::Position => self.position = m.mul_matrix(&self.position),
            MtxMode::PosVector => {
                self.position = m.mul_matrix(&self.position);
                self.vector = m.mul_matrix(&self.vector);
            }
            MtxMode::Texture => self.texture = m.mul_matrix(&self.texture),
        }
    }

    /// `MTX_PUSH` — save current onto its stack.
    pub fn push(&mut self) {
        match self.mode {
            MtxMode::Projection => {
                if self.projection_sp != 0 {
                    self.overflow = true;
                }
                self.projection_saved = self.projection;
                self.projection_sp = self.projection_sp.wrapping_add(1) & 1;
            }
            MtxMode::Texture => {
                if self.texture_saved_valid {
                    self.overflow = true; // already-saved -> overwrite-with-overflow
                }
                self.texture_saved = self.texture;
                self.texture_saved_valid = true;
            }
            MtxMode::Position | MtxMode::PosVector => {
                let idx = (self.position_sp & 0x1F) as usize;
                if self.position_sp >= 31 {
                    self.overflow = true;
                }
                self.position_stack[idx] = self.position;
                self.vector_stack[idx] = self.vector;
                self.position_sp = self.position_sp.wrapping_add(1) & 0x3F;
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
            MtxMode::Projection => {
                if self.projection_sp == 0 {
                    self.overflow = true;
                }
                self.projection_sp = self.projection_sp.wrapping_sub(1) & 1;
                self.projection = self.projection_saved;
            }
            MtxMode::Texture => {
                if !self.texture_saved_valid {
                    self.overflow = true;
                    return;
                }
                self.texture = self.texture_saved;
                self.texture_saved_valid = false;
            }
            MtxMode::Position | MtxMode::PosVector => {
                if signed_count < -30 {
                    self.overflow = true;
                }
                let raw_sp = (self.position_sp as i32 - signed_count).rem_euclid(64);
                self.position_sp = raw_sp as u8;
                if self.position_sp >= 31 {
                    self.overflow = true;
                }
                let sp = (self.position_sp & 0x1F) as usize;
                self.position = self.position_stack[sp];
                self.vector = self.vector_stack[sp];
            }
        }
    }

    /// `MTX_STORE` — copy current to a specific stack slot (0..30 for the
    /// position pair). For projection/texture this writes the single saved
    /// slot without changing the stack pointer.
    pub fn store(&mut self, slot: u32) {
        match self.mode {
            MtxMode::Projection => {
                self.projection_saved = self.projection;
            }
            MtxMode::Texture => {
                self.texture_saved = self.texture;
                self.texture_saved_valid = true;
            }
            MtxMode::Position | MtxMode::PosVector => {
                let idx = (slot & 0x1F) as usize;
                if idx == 31 {
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
                self.projection = self.projection_saved;
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
                if idx == 31 {
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

    /// GXSTAT bit 15 is write-one-to-clear. Hardware also resets the
    /// projection stack pointer when acknowledging the matrix-stack error.
    pub fn clear_overflow_error(&mut self) {
        self.overflow = false;
        self.projection_sp = 0;
        self.texture_saved_valid = false;
    }
}

impl Default for MatrixStacks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::matrix::ONE;
    use super::*;

    #[test]
    fn test_load_and_mult_in_projection_mode() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Projection);
        let m = Matrix::identity().mul_translate(3 * ONE, 0, 0);
        s.load(m);
        assert_eq!(s.projection.at(3, 0), 3 * ONE);
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
    fn test_pos_vector_scale_updates_only_position() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::PosVector);
        s.scale(2 * ONE, 3 * ONE, 4 * ONE);

        assert_eq!(s.position.at(0, 0), 2 * ONE);
        assert_eq!(s.position.at(1, 1), 3 * ONE);
        assert_eq!(s.position.at(2, 2), 4 * ONE);
        assert_eq!(s.vector, Matrix::identity());
    }

    #[test]
    fn test_mult_premultiplies_current_matrix() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Position);
        s.load(Matrix::identity().mul_translate(ONE, 0, 0));

        s.mult(Matrix::identity().mul_scale(2 * ONE, 2 * ONE, 2 * ONE));

        let r = s.position.mul_vec4([0, 0, 0, ONE]);
        assert_eq!(
            r[0], ONE,
            "C = scale * translate keeps translation unscaled"
        );
    }

    #[test]
    fn test_scale_command_premultiplies_current_matrix() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Position);
        s.load(Matrix::identity().mul_translate(ONE, 0, 0));

        s.scale(2 * ONE, 2 * ONE, 2 * ONE);

        let r = s.position.mul_vec4([0, 0, 0, ONE]);
        assert_eq!(r[0], ONE, "MTX_SCALE uses C = M * C");
    }

    #[test]
    fn test_pos_vector_translate_preserves_separate_matrices() {
        let mut s = MatrixStacks::new();
        s.position = Matrix::identity().mul_scale(2 * ONE, 2 * ONE, 2 * ONE);
        s.vector = Matrix::identity().mul_scale(3 * ONE, 3 * ONE, 3 * ONE);
        s.set_mode(MtxMode::PosVector);
        s.translate(ONE, 0, 0);

        assert_eq!(s.position.at(0, 0), 2 * ONE);
        assert_eq!(s.vector.at(0, 0), 3 * ONE);
        assert_eq!(s.position.at(3, 0), 2 * ONE);
        assert_eq!(s.vector.at(3, 0), 3 * ONE);
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
    fn test_position_mode_stack_ops_preserve_vector_matrix() {
        let mut s = MatrixStacks::new();
        s.vector = Matrix::identity().mul_scale(2 * ONE, 3 * ONE, 4 * ONE);
        s.set_mode(MtxMode::Position);

        s.push();
        let saved_vector = s.vector;
        s.vector = Matrix::identity().mul_translate(5 * ONE, 0, 0);

        s.pop(1);

        assert_eq!(s.vector, saved_vector);
    }

    #[test]
    fn test_position_mode_store_restore_preserves_vector_matrix() {
        let mut s = MatrixStacks::new();
        s.vector = Matrix::identity().mul_scale(2 * ONE, 3 * ONE, 4 * ONE);
        s.set_mode(MtxMode::Position);

        s.store(4);
        let saved_vector = s.vector;
        s.vector = Matrix::identity().mul_translate(5 * ONE, 0, 0);

        s.restore(4);

        assert_eq!(s.vector, saved_vector);
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
        for _ in 0..31 {
            s.push();
        }
        assert_eq!(s.position_sp, 31);
        // 32nd push stores to the mirrored/error entry 31, flags overflow,
        // and advances the 6-bit pointer.
        s.push();
        assert!(s.overflow);
        assert_eq!(s.position_sp, 32);
    }

    #[test]
    fn test_pop_signed_offset() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Position);
        for _ in 0..5 {
            s.push();
        }
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
    fn test_store_restore_slot_31_sets_overflow_without_accessing_entry() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Position);
        let target = Matrix::identity().mul_translate(31 * ONE, 0, 0);
        let original_slot_31 = s.position_stack[31];
        s.load(target);

        s.store(31);
        assert!(s.overflow);
        assert_eq!(s.position_stack[31], original_slot_31);
        s.identity();
        s.overflow = false;
        s.restore(31);

        assert!(s.overflow);
        let r = s.position.mul_vec4([0, 0, 0, ONE]);
        assert_eq!(r[0], 0);
    }

    #[test]
    fn test_pop_into_upper_mirror_sets_overflow_and_uses_lower_entry() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Position);
        let target = Matrix::identity().mul_translate(2 * ONE, 0, 0);
        s.load(target);
        s.store(2);
        s.position_sp = 30;

        s.pop(0x3E); // -2, moves pointer to 32, mirrored entry 0.

        assert!(s.overflow);
        assert_eq!(s.position_sp, 32);
        assert_eq!(s.position, s.position_stack[0]);
    }

    #[test]
    fn test_clear_overflow_resets_projection_stack_level() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Projection);
        s.push();
        s.set_mode(MtxMode::Texture);
        s.push();
        s.overflow = true;

        s.clear_overflow_error();

        assert!(!s.overflow);
        assert_eq!(s.projection_sp, 0);
        assert!(!s.texture_saved_valid);
    }

    #[test]
    fn test_projection_store_restore_do_not_change_stack_pointer() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Projection);

        let stored = Matrix::identity().mul_translate(3 * ONE, 0, 0);
        s.load(stored);
        s.store(0);
        assert_eq!(s.projection_sp, 0);

        s.identity();
        s.restore(0);
        assert_eq!(s.projection_sp, 0);
        assert_eq!(s.projection, stored);
    }

    #[test]
    fn test_projection_push_pop_updates_stack_pointer() {
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Projection);

        let stored = Matrix::identity().mul_translate(4 * ONE, 0, 0);
        s.load(stored);
        s.push();
        assert_eq!(s.projection_sp, 1);

        s.identity();
        s.pop(0);
        assert_eq!(s.projection_sp, 0);
        assert_eq!(s.projection, stored);
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

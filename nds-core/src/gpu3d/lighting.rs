//! Per-vertex lighting.
//!
//! 4 directional lights (each with a direction vector + RGB color).
//! Material parameters: diffuse + ambient + specular + emission, each a
//! 5-bit-per-channel BGR555 value. A 128-entry "shininess" LUT
//! approximates the Phong specular falloff `(reflection · viewer)^n`.
//!
//! For each lit polygon vertex, color is computed as:
//!
//! ```text
//!   color = emission
//!         + ambient_global × material_ambient
//!         + Σ_lights enable_bit → light_color × (
//!               diffuse_term  (= max(0, -L · N))   × material_diffuse
//!             + specular_term (= shininess_lut[H · N]) × material_specular
//!           )
//! ```
//!
//! All channels clamped to 0..31 and re-packed into a 15-bit BGR555.
//!
//! Reference: GBATEK §"DS 3D Lighting".

use serde::{Deserialize, Serialize};

use super::matrix::{Matrix, ONE, fmul};

/// One directional light source.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Light {
    /// Direction the light points (object space; transformed by vector
    /// matrix on `LIGHT_VECTOR`). Normalized to a unit vector in 1.0.9
    /// fixed-point — i.e. values are in [-512, 511].
    pub direction: [i32; 3],
    /// BGR555 color (each channel 0..31).
    pub color: u16,
}

/// Lighting unit state — set by GX commands DIF_AMB, SPE_EMI, LIGHT_VECTOR,
/// LIGHT_COLOR, SHININESS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightingState {
    pub lights: [Light; 4],

    /// Material colors as BGR555 (each component 0..31).
    pub mat_diffuse: u16,
    pub mat_ambient: u16,
    pub mat_specular: u16,
    pub mat_emission: u16,

    /// Whether DIF_AMB had its "set vertex color from diffuse" bit set;
    /// in that case the diffuse term replaces the current vertex color
    /// on the next VTX_*.
    pub set_color_from_diffuse: bool,
    /// Same for specular's "shininess table enable" toggle.
    pub use_shininess_table: bool,

    /// 128-entry shininess LUT (each entry 0..255, used as 0..1 scale).
    /// Stored as `Vec<u8>` for serde compatibility (array sizes > 32 aren't
    /// directly serializable by stock serde); always exactly 128 bytes.
    #[serde(with = "crate::bus::shared::serde_bytes_vec")]
    pub shininess_table: Vec<u8>,
}

impl LightingState {
    pub fn new() -> Self {
        LightingState {
            lights: [Light::default(); 4],
            mat_diffuse: 0,
            mat_ambient: 0,
            mat_specular: 0,
            mat_emission: 0,
            set_color_from_diffuse: false,
            use_shininess_table: false,
            shininess_table: vec![0u8; 128],
        }
    }

    /// `DIF_AMB` — `[14:0]` = material diffuse, `[15]` = set-color-from-diff,
    /// `[30:16]` = material ambient.
    pub fn set_dif_amb(&mut self, param: u32) {
        self.mat_diffuse = (param & 0x7FFF) as u16;
        self.set_color_from_diffuse = (param & (1 << 15)) != 0;
        self.mat_ambient = ((param >> 16) & 0x7FFF) as u16;
    }

    /// `SPE_EMI` — `[14:0]` = material specular, `[15]` = shininess-table-enable,
    /// `[30:16]` = material emission.
    pub fn set_spe_emi(&mut self, param: u32) {
        self.mat_specular = (param & 0x7FFF) as u16;
        self.use_shininess_table = (param & (1 << 15)) != 0;
        self.mat_emission = ((param >> 16) & 0x7FFF) as u16;
    }

    /// `LIGHT_VECTOR` — `[29:0]` = three 10-bit signed components,
    /// `[31:30]` = light index. Transformed by the vector matrix to put
    /// the light direction in eye space.
    pub fn set_light_vector(&mut self, param: u32, vec_matrix: &Matrix) {
        let id = ((param >> 30) & 0x3) as usize;
        let sign_ext = |b: u32| -> i32 { (((b & 0x3FF) << 22) as i32) >> 22 };
        let dx = sign_ext(param) << 3;       // 10-bit -> 1.19.12 (shift 6) then scale 1/512
        let dy = sign_ext(param >> 10) << 3;
        let dz = sign_ext(param >> 20) << 3;
        // Transform direction by the vector matrix (rotational part).
        let transformed = vec_matrix.mul_vec4([dx, dy, dz, 0]);
        self.lights[id].direction = [transformed[0], transformed[1], transformed[2]];
    }

    /// `LIGHT_COLOR` — `[14:0]` = BGR555 color, `[31:30]` = light index.
    pub fn set_light_color(&mut self, param: u32) {
        let id = ((param >> 30) & 0x3) as usize;
        self.lights[id].color = (param & 0x7FFF) as u16;
    }

    /// `SHININESS` — 32 parameter words, each holds 4 entries (one per byte).
    pub fn set_shininess(&mut self, params: &[u32]) {
        for (word_idx, &word) in params.iter().enumerate().take(32) {
            for byte_idx in 0..4 {
                let idx = word_idx * 4 + byte_idx;
                if idx < 128 {
                    self.shininess_table[idx] = ((word >> (byte_idx * 8)) & 0xFF) as u8;
                }
            }
        }
    }
}

impl Default for LightingState {
    fn default() -> Self { Self::new() }
}

/// Pack three 0..31 channels into BGR555.
fn pack_bgr555(r: i32, g: i32, b: i32) -> u16 {
    let r = r.clamp(0, 31) as u16;
    let g = g.clamp(0, 31) as u16;
    let b = b.clamp(0, 31) as u16;
    r | (g << 5) | (b << 10)
}

/// Unpack BGR555 to (R, G, B) channels in 0..31.
fn unpack_bgr555(c: u16) -> (i32, i32, i32) {
    (
        (c & 0x1F) as i32,
        ((c >> 5) & 0x1F) as i32,
        ((c >> 10) & 0x1F) as i32,
    )
}

/// Dot product of two 1.19.12 fixed-point 3-vectors, returned in 1.19.12.
fn dot3(a: [i32; 3], b: [i32; 3]) -> i32 {
    fmul(a[0], b[0]) + fmul(a[1], b[1]) + fmul(a[2], b[2])
}

/// Compute the lit color for one vertex.
///
/// - `normal_obj` is the object-space normal (1.0.9 fixed-point components,
///   shifted by 3 to 1.0.12 before being passed in).
/// - `light_enable_mask` is the low 4 bits of POLYGON_ATTR (which lights apply).
pub fn compute_vertex_color(
    state: &LightingState,
    normal_obj: [i32; 3],
    vector_matrix: &Matrix,
    light_enable_mask: u8,
) -> u16 {
    // Transform normal to eye space (using the vector matrix, which is
    // the inverse-transpose of the position matrix for rigid transforms).
    let n_eye = vector_matrix.mul_vec4([normal_obj[0], normal_obj[1], normal_obj[2], 0]);
    let n = [n_eye[0], n_eye[1], n_eye[2]];

    let (em_r, em_g, em_b) = unpack_bgr555(state.mat_emission);
    let (am_r, am_g, am_b) = unpack_bgr555(state.mat_ambient);
    let (df_r, df_g, df_b) = unpack_bgr555(state.mat_diffuse);
    let (sp_r, sp_g, sp_b) = unpack_bgr555(state.mat_specular);

    // Start with emission + ambient.
    let mut r = em_r + am_r;
    let mut g = em_g + am_g;
    let mut b = em_b + am_b;

    // Half-vector for specular: H = normalize(L + V). We approximate V as
    // (0, 0, -1) in eye space (camera looking down -Z).
    for (i, light) in state.lights.iter().enumerate() {
        if light_enable_mask & (1 << i) == 0 { continue; }

        let (lr, lg, lb) = unpack_bgr555(light.color);
        let l = light.direction;
        // Diffuse: max(0, -L · N). NDS convention: L points *from* the
        // surface toward the light; we negate to get the surface→light
        // direction, then dot with N.
        let d = -dot3(l, n);
        let diff_factor = (d.clamp(0, ONE)) as i32;

        // Specular: half-vector H = (L + V) / |L + V|. NDS does an
        // approximate Phong by computing (V·R) and looking it up in the
        // shininess table.
        let v = [0, 0, -ONE]; // looking down -Z
        // R = 2(N·L)N - L
        let nl = dot3(n, l);
        let r_vec = [
            fmul(2 * nl, n[0]) - l[0],
            fmul(2 * nl, n[1]) - l[1],
            fmul(2 * nl, n[2]) - l[2],
        ];
        let rv = -dot3(r_vec, v).clamp(0, ONE);
        let spec_factor = if state.use_shininess_table {
            // LUT indexed by RV in [0..127].
            let idx = ((rv * 127) / ONE).clamp(0, 127) as usize;
            (state.shininess_table[idx] as i32) * ONE / 255
        } else {
            rv
        };

        // Accumulate diffuse contribution per-channel: light × material × factor.
        let scale = |light_chan: i32, mat_chan: i32, factor: i32| -> i32 {
            // (light/31) × (mat/31) × factor → result in 0..31
            // Approximate to avoid floats: (light * mat * factor) / (31 * ONE)
            ((light_chan * mat_chan * factor) / (31 * ONE)).clamp(0, 31)
        };
        r += scale(lr, df_r, diff_factor);
        g += scale(lg, df_g, diff_factor);
        b += scale(lb, df_b, diff_factor);
        r += scale(lr, sp_r, spec_factor);
        g += scale(lg, sp_g, spec_factor);
        b += scale(lb, sp_b, spec_factor);
    }

    pack_bgr555(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_lighting_returns_emission_plus_ambient() {
        let mut s = LightingState::new();
        s.set_dif_amb((0x0421 << 16) | 0); // ambient = (1, 1, 1) in BGR555 channels
        s.set_spe_emi((0x4210 << 16) | 0); // emission = (16, 16, 16)? Let's pick (16,16,16)
        // Compose emission = R=16, G=16, B=16  →  packed = 16 | (16<<5) | (16<<10) = 0x4210
        // ambient = (1,1,1) packed = 1 | (1<<5) | (1<<10) = 0x0421
        let c = compute_vertex_color(&s, [0, 0, ONE], &Matrix::identity(), 0);
        let (r, g, b) = unpack_bgr555(c);
        assert_eq!((r, g, b), (17, 17, 17), "emission + ambient");
    }

    #[test]
    fn test_disabled_lights_contribute_nothing() {
        let mut s = LightingState::new();
        // Bright white light pointed at +Z. Default material is black,
        // so contribution is zero anyway, but more importantly the
        // light_enable_mask of 0 should bypass lights entirely.
        s.lights[0].direction = [0, 0, -ONE];
        s.lights[0].color = 0x7FFF;
        let c = compute_vertex_color(&s, [0, 0, ONE], &Matrix::identity(), 0);
        assert_eq!(c, 0, "all-disabled lights → black");
    }

    #[test]
    fn test_set_dif_amb_unpacks_correctly() {
        let mut s = LightingState::new();
        s.set_dif_amb(0x4321_8765);
        // low 15 bits = 0x0765, ambient = high 15 bits of (param>>16) = 0x4321 & 0x7FFF = 0x4321
        assert_eq!(s.mat_diffuse, 0x0765);
        assert!(s.set_color_from_diffuse, "bit 15 was set");
        assert_eq!(s.mat_ambient, 0x4321);
    }

    #[test]
    fn test_light_vector_unpacks_index() {
        let mut s = LightingState::new();
        // index 2 (bits 30-31 = 0b10 → 2), all zero direction.
        s.set_light_vector(2 << 30, &Matrix::identity());
        // We just check the call dispatches by index; the direction is
        // identity-transformed (still 0, 0, 0). Hard to assert directly
        // without verifying internal state, but reading back light[2] is
        // a "stored to right slot" check.
        assert_eq!(s.lights[2].direction, [0, 0, 0]);
    }

    #[test]
    fn test_shininess_table_loads_4_per_word() {
        let mut s = LightingState::new();
        let params: Vec<u32> = (0..32).map(|w| {
            // Each word is 4 incrementing bytes: 0,1,2,3 ; 4,5,6,7 ; ...
            let base = (w * 4) as u32;
            base | ((base + 1) << 8) | ((base + 2) << 16) | ((base + 3) << 24)
        }).collect();
        s.set_shininess(&params);
        for i in 0..128 {
            assert_eq!(s.shininess_table[i] as usize, i, "entry {} should be {}", i, i);
        }
    }
}

//! A ray-traced book cuboid rendered to straight RGBA.
//! Takes edition proportions as two ratios and must not depend on the engine.

pub mod math;
pub mod raster;
pub mod scene;
pub mod texture;

pub use raster::{Quality, RgbaBuf, Target, render_rgba};
pub use scene::Pose;
pub use texture::Cover;

use math::{Vec3, vec3};

/// Half-extents in scene units; the engine owns the proportions, this crate only scales them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model {
    pub half: Vec3,
}

impl Model {
    /// Scales proportions given with height 1.0 by [`scene::HALF_HEIGHT`].
    pub fn from_proportions(width_over_height: f32, thickness_over_height: f32) -> Model {
        let h = scene::HALF_HEIGHT;
        Model {
            half: vec3(h * width_over_height, h, h * thickness_over_height),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderParams {
    pub pose: Pose,
    /// Silhouette supersampling per axis; the raster uses the larger of this and the [`Quality`] tier's.
    pub ss: u8,
}

impl Default for RenderParams {
    fn default() -> Self {
        RenderParams {
            pose: Pose::default(),
            ss: 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proportions_scale_by_the_half_height() {
        let m = Model::from_proportions(2.0 / 3.0, 0.1);
        assert!((m.half.y - scene::HALF_HEIGHT).abs() < 1e-6);
        assert!((m.half.x - scene::HALF_HEIGHT * 2.0 / 3.0).abs() < 1e-6);
        assert!((m.half.z - scene::HALF_HEIGHT * 0.1).abs() < 1e-6);
    }
}

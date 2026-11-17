//! The book object: one ray-traced cuboid, traced into straight RGBA.
//!
//! Extracted from the TUI's `render3d/` so a second frontend can draw the
//! *same* book. The renderer is deliberately frozen as it is: `raster.rs`
//! already emitted RGBA, so any frontend can display a Rust-rendered image and
//! the renderer survives a change of frontend intact. What came across is the
//! physics and the pixel raster:
//!
//! - [`math`] — the dozen vector/matrix operations the tracer needs;
//! - [`scene`] — the camera rig, the slab intersection, the lighting, [`Pose`];
//! - [`texture`] — cover decode, the accent's luma clamp, the procedural plate;
//! - [`raster`] — [`render_rgba`]: one ray per pixel plus a silhouette re-trace.
//!
//! What did **not** come across, on purpose: block glyphs, kitty, the terminal
//! budgets that size a frame for a pty, and any knowledge of a `Book`. The
//! engine decides what shape an edition is (`bookmole::edition`) and
//! states it as two ratios of height; a frontend reads them off whatever it
//! holds — a `Book` in the TUI, a `BookDto` in a client of `bookmole-api` —
//! and hands them to [`Model::from_proportions`]. That is what lets an
//! API-only frontend link this crate and still never link the engine.

pub mod math;
pub mod raster;
pub mod scene;
pub mod texture;

pub use raster::{Quality, RgbaBuf, Target, render_rgba};
pub use scene::Pose;
pub use texture::Cover;

use math::{Vec3, vec3};

/// This scene's half-extents for one edition, in scene units.
///
/// The *decision* about what shape a book is lives in the engine, so that a
/// shelf drawn by some other frontend and this ray tracer agree about how fat
/// *Infinite Jest* is. What is left here is the scaling, which is the
/// renderer's alone: the engine states proportions with height fixed at 1.0,
/// and [`scene::HALF_HEIGHT`] is this camera rig's idea of how tall that is.
/// **Do not push a scene constant back down into that derivation.**
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model {
    pub half: Vec3,
}

impl Model {
    /// The edition's proportions — `EditionShape`'s / `EditionShapeDto`'s
    /// `width_over_height` and `thickness_over_height` — scaled by this scene's
    /// height.
    pub fn from_proportions(width_over_height: f32, thickness_over_height: f32) -> Model {
        let h = scene::HALF_HEIGHT;
        Model {
            half: vec3(h * width_over_height, h, h * thickness_over_height),
        }
    }
}

/// The knobs the pixel raster reads.
///
/// The TUI's own `RenderParams` carries two more — the block-glyph family and
/// whether the book is animating — and both are terminal policy, so they stay
/// there; it converts into this one at the call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderParams {
    pub pose: Pose,
    /// Supersampling factor per axis on the silhouette re-trace; the raster
    /// takes the larger of this and the [`Quality`] tier's own.
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

    /// The reference trade paperback the scene tests are written against is a
    /// 2:3 cover; the factor between proportions and scene units is the half
    /// height, on all three axes.
    #[test]
    fn proportions_scale_by_the_half_height() {
        let m = Model::from_proportions(2.0 / 3.0, 0.1);
        assert!((m.half.y - scene::HALF_HEIGHT).abs() < 1e-6);
        assert!((m.half.x - scene::HALF_HEIGHT * 2.0 / 3.0).abs() < 1e-6);
        assert!((m.half.z - scene::HALF_HEIGHT * 0.1).abs() < 1e-6);
    }
}

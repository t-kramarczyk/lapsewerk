//! Lapsewerk core: the parts of a time-lapse workflow that do not depend on how a frame is developed.
//!
//! - [`sequence`]: frames, capture times, interval statistics and gaps.
//! - [`exposure`]: exposure values from camera settings and the compensation that cancels the camera's
//!   exposure steps during a day-to-night ("holy grail") ramp.
//! - [`keyframes`]: develop settings set on a few frames, interpolated over every frame.
//! - [`deflicker`]: brightness measurement of developed frames and the per-frame correction that removes
//!   flicker while keeping the slow, intended change of light.
//! - [`smooth`]: robust smoothing used by the above.
//!
//! Every brightness quantity is in stops (log2), so corrections add up: the exposure of a frame is its
//! interpolated keyframe exposure, plus its ramp compensation, plus its deflicker correction.

pub mod astro;
pub mod blacks;
pub mod daynight;
pub mod defects;
pub mod deflicker;
pub mod dust;
pub mod exposure;
pub mod framing;
pub mod hdr;
pub mod keyframes;
pub mod quality;
pub mod regional;
pub mod sequence;
pub mod skymask;
pub mod smooth;
pub mod stabilize;
pub mod temporal;
pub mod tween;

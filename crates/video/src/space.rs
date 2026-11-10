//! How much room an export takes, and whether the destination has it: a night's render of 2000
//! 4K DPX frames needs about 66 GB, and a full disk at frame 1500 is found out too late.

use std::path::Path;

use crate::frames::FrameFormat;
use crate::{Codec, VideoSettings};

/// Bytes `count` frames of `settings` take: as files in `frames` format, or (`None`) as a movie.
/// An estimate (compressed formats vary with the picture), on the generous side.
#[must_use]
pub fn estimate_bytes(settings: &VideoSettings, frames: Option<FrameFormat>, count: u64) -> u64 {
    let px = (settings.width as u64).saturating_mul(settings.height as u64);
    let per_frame = match frames {
        // Deflate on 16-bit photos keeps about three quarters.
        Some(FrameFormat::Tiff) => px.saturating_mul(6).saturating_mul(3) / 4,
        Some(FrameFormat::Dpx) => px.saturating_mul(4).saturating_add(2048),
        // Half floats, ZIP: about 60 %.
        Some(FrameFormat::Exr) => px.saturating_mul(6).saturating_mul(3) / 5,
        None => {
            // Bits a pixel and frame: Apple's target rates for ProRes; HEVC as set or 0.2.
            let bits = match settings.codec {
                Codec::ProRes422Hq => 3.6,
                Codec::ProRes422 => 2.4,
                Codec::ProRes4444 => 5.4,
                Codec::Hevc => {
                    let fps = f64::from(settings.fps_num) / f64::from(settings.fps_den.max(1));
                    settings
                        .bitrate
                        .map_or(0.2, |b| b as f64 / (px.max(1) as f64 * fps.max(1.0)))
                }
            };
            (px as f64 * bits / 8.0) as u64
        }
    };
    // A movie's first frame is a key frame of several times the others' size, and the container
    // adds its own: about 2 bits a pixel once, which a short movie (a test of 15 frames) would
    // otherwise exceed the estimate by.
    let once = if frames.is_none() {
        px.saturating_mul(2) / 8
    } else {
        0
    };
    per_frame.saturating_mul(count).saturating_add(once)
}

/// Bytes free where `path` goes (the nearest folder above it that exists); `None` when unknown.
#[must_use]
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut p = Some(path);
    while let Some(d) = p {
        if d.is_dir() {
            return fs2::available_space(d).ok();
        }
        p = d.parent();
    }
    None
}

/// Whether `needed` bytes fit where `path` goes (with 2 % to spare). `Err` names both figures.
///
/// # Errors
/// When the destination has less room than the export needs.
pub fn check(path: &Path, needed: u64) -> Result<(), SpaceShort> {
    match free_bytes(path) {
        Some(free) if free < needed.saturating_add(needed / 50) => Err(SpaceShort { needed, free }),
        _ => Ok(()),
    }
}

/// Not enough room for an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceShort {
    pub needed: u64,
    pub free: u64,
}

impl std::fmt::Display for SpaceShort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the export needs about {} and {} is free there",
            gigabytes(self.needed),
            gigabytes(self.free)
        )
    }
}

/// Bytes as gigabytes for people ("12.3 GB", "850 MB").
#[must_use]
pub fn gigabytes(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Transfer;

    fn uhd(codec: Codec) -> VideoSettings {
        VideoSettings {
            width: 3840,
            height: 2160,
            fps_num: 25,
            fps_den: 1,
            codec,
            bitrate: None,
            transfer: Transfer::Srgb,
            timecode: None,
            drop_frame: false,
        }
    }

    #[test]
    fn exports_are_sized_as_their_formats_take() {
        // 2000 4K DPX frames: 66 GB, as a pro measured.
        let dpx = estimate_bytes(&uhd(Codec::Hevc), Some(FrameFormat::Dpx), 2000);
        assert!((dpx as f64 / 1e9 - 66.4).abs() < 0.5, "{dpx}");
        // ProRes 422 HQ 4K at 25 fps: about 750 Mbit/s, so a minute is about 5.6 GB.
        let hq = estimate_bytes(&uhd(Codec::ProRes422Hq), None, 25 * 60);
        assert!((hq as f64 / 1e9 - 5.6).abs() < 0.3, "{hq}");
        // HEVC at a set rate: the rate times the length.
        let mut hevc = uhd(Codec::Hevc);
        hevc.bitrate = Some(50_000_000);
        let b = estimate_bytes(&hevc, None, 25 * 60);
        assert!((b as f64 / 1e9 - 0.375).abs() < 0.01, "{b}");
        assert_eq!(gigabytes(66_400_000_000), "66.4 GB");
        assert_eq!(gigabytes(850_000_000), "850 MB");
    }

    #[test]
    fn space_is_checked_where_the_export_goes() {
        let dir = std::env::temp_dir();
        // Somewhere below the temporary folder that does not exist yet: its free space counts.
        let target = dir
            .join(format!("lapsewerk-space-{}", std::process::id()))
            .join("a/b");
        assert!(free_bytes(&target).is_some());
        assert!(check(&target, 1).is_ok());
        let short = check(&target, u64::MAX / 2).unwrap_err();
        assert!(short.to_string().contains("free there"), "{short}");
    }
}

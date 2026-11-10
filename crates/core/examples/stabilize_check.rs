//! Checks stabilization against a known truth: real frames (raw dumps as for `denoise_check`) are
//! cut out at known, shaking offsets (whole pixels, as a shaky tripod would move them), then the
//! shifts are measured and the shake corrected. Reports the error of each measured step and the
//! shake left after correction.
//!
//! `cargo run --release -p lapsewerk-core --example stabilize_check -- <dir with f0.raw…f8.raw>`

use std::error::Error;
use std::path::Path;

use lapsewerk_core::regional::srgb_to_linear;
use lapsewerk_core::stabilize::{self, Thumb};

fn load(path: &Path) -> Result<(usize, usize, Vec<u16>), Box<dyn Error>> {
    let bytes = std::fs::read(path)?;
    let word = |i: usize| -> Result<usize, Box<dyn Error>> {
        let b: [u8; 4] = bytes.get(i..i + 4).ok_or("short file")?.try_into()?;
        Ok(u32::from_le_bytes(b) as usize)
    };
    let (w, h) = (word(0)?, word(4)?);
    let data = bytes
        .get(8..)
        .ok_or("short")?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect();
    Ok((w, h, data))
}

fn main() -> Result<(), Box<dyn Error>> {
    let dir = std::env::args().nth(1).ok_or("usage: stabilize_check <dir>")?;
    let dir = Path::new(&dir);
    // Shake in full-size pixels: up to ±12 px, varying from frame to frame, on 3 frames each of the 9.
    let shake: Vec<(isize, isize)> = (0..27)
        .map(|i| {
            let a = i as f64 * 2.4;
            (
                (12.0 * a.sin()).round() as isize,
                (9.0 * (a * 1.7).cos()).round() as isize,
            )
        })
        .collect();
    let margin = 16usize;
    let mut thumbs = Vec::new();
    for (k, (sx, sy)) in shake.iter().enumerate() {
        let (w, h, data) = load(&dir.join(format!("f{}.raw", k / 3)))?;
        let (cw, ch) = (w - 2 * margin, h - 2 * margin);
        let (ox, oy) = ((margin as isize + sx) as usize, (margin as isize + sy) as usize);
        let mut lin = Vec::with_capacity(cw * ch * 3);
        for y in 0..ch {
            for x in 0..cw {
                let i = ((y + oy) * w + (x + ox)) * 3;
                for c in 0..3 {
                    lin.push(srgb_to_linear(
                        f32::from(data.get(i + c).copied().unwrap_or(0)) / 65535.0,
                    ));
                }
            }
        }
        thumbs.push((
            Thumb::from_linear_rgb(&lin, cw, ch, [0.2126, 0.7152, 0.0722]).ok_or("thumb")?,
            cw,
            ch,
        ));
    }
    let mut worst = 0.0f64;
    let mut steps = Vec::new();
    for (k, pair) in thumbs.windows(2).enumerate() {
        let [(a, cw, ch), (b, _, _)] = pair else { continue };
        let (dx, dy, peak) = stabilize::shift(a, b).ok_or("shift")?;
        // The cut-out moved by +s, so the content moved by −s.
        let (Some(s0), Some(s1)) = (shake.get(k), shake.get(k + 1)) else {
            continue;
        };
        let (tx, ty) = (s1.0 - s0.0, s1.1 - s0.1);
        let (fx, fy) = (
            dx * *cw as f64 / stabilize::SIZE as f64,
            dy * *ch as f64 / stabilize::SIZE as f64,
        );
        let err = (fx + tx as f64).hypot(fy + ty as f64);
        worst = worst.max(err);
        println!(
            "step {k:>2}: true ({:>3}, {:>3}) px, measured ({:>7.2}, {:>7.2}), error {err:.2} px, peak {peak:.2}",
            -tx, -ty, fx, fy
        );
        steps.push(Some((dx, dy)));
    }
    let &(_, cw, ch) = thumbs.first().ok_or("no frames")?;
    println!("largest error of a step: {worst:.2} px (frames {cw}×{ch})");
    let c = stabilize::corrections(&steps, 12.0);
    // Position of the content after correction, in full pixels: should be (nearly) still.
    let (cw, ch) = (cw as f64, ch as f64);
    let s = stabilize::SIZE as f64;
    let after: Vec<(f64, f64)> = shake
        .iter()
        .zip(&c)
        .map(|((sx, sy), (cx, cy))| (-*sx as f64 + cx * cw / s, -*sy as f64 + cy * ch / s))
        .collect();
    let rms = |v: &[(f64, f64)]| {
        let (mx, my) = (
            v.iter().map(|p| p.0).sum::<f64>() / v.len() as f64,
            v.iter().map(|p| p.1).sum::<f64>() / v.len() as f64,
        );
        (v.iter()
            .map(|p| (p.0 - mx).powi(2) + (p.1 - my).powi(2))
            .sum::<f64>()
            / v.len() as f64)
            .sqrt()
    };
    let before: Vec<(f64, f64)> = shake.iter().map(|(x, y)| (-*x as f64, -*y as f64)).collect();
    println!(
        "shake RMS before {:.2} px, after {:.2} px",
        rms(&before),
        rms(&after)
    );
    Ok(())
}

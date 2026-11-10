//! Checks temporal noise reduction against a known truth: clean rendered frames (raw dumps:
//! `u32 width, u32 height, width × height × 3 u16`, little-endian) get Gaussian noise of a chosen
//! strength; the denoised middle frame is compared with the clean one, separately where the scene
//! is still and where it moves.
//!
//! `cargo run --release -p lapsewerk-core --example denoise_check -- <dir with f0.raw…f8.raw> [sigma] [static]`
//!
//! `static` uses the middle frame nine times: a perfectly still scene, where the best possible gain
//! is √(2·radius + 1).

use std::error::Error;
use std::path::Path;

use lapsewerk_core::temporal::{self, DenoiseParams, Image16};

fn load(path: &Path) -> Result<Image16, Box<dyn Error>> {
    let bytes = std::fs::read(path)?;
    let word = |i: usize| -> Result<usize, Box<dyn Error>> {
        let b: [u8; 4] = bytes.get(i..i + 4).ok_or("short file")?.try_into()?;
        Ok(u32::from_le_bytes(b) as usize)
    };
    let (w, h) = (word(0)?, word(4)?);
    let data: Vec<u16> = bytes
        .get(8..)
        .ok_or("short file")?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect();
    Image16::new(w, h, data).ok_or_else(|| "size does not match".into())
}

/// Deterministic Gaussian noise (xorshift + Box–Muller).
struct Rng(u64);
impl Rng {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        let (u, v) = (self.uniform(), self.uniform());
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    let dir = Path::new(args.get(1).ok_or("usage: denoise_check <dir> [sigma]")?);
    let sigma: f64 = args.get(2).map_or(Ok(0.02), |s| s.parse())?;
    let still_scene = args.get(3).is_some_and(|a| a == "static");
    let clean: Vec<Image16> = (0..9)
        .map(|k| load(&dir.join(format!("f{}.raw", if still_scene { 4 } else { k }))))
        .collect::<Result<_, _>>()?;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let noisy: Vec<Image16> = clean
        .iter()
        .map(|c| Image16 {
            width: c.width,
            height: c.height,
            data: c
                .data
                .iter()
                .map(|v| {
                    (f64::from(*v) + sigma * 65535.0 * rng.gauss())
                        .round()
                        .clamp(0.0, 65535.0) as u16
                })
                .collect(),
        })
        .collect();
    let truth = clean.get(4).ok_or("need 9 frames")?;
    let noisy_mid = noisy.get(4).ok_or("need 9 frames")?;
    // Moving: the clean scene changes by more than 1 % somewhere in the 9 frames; still: by under 0.2 %.
    let px = truth.data.len() / 3;
    let change: Vec<f64> = (0..px)
        .map(|p| {
            let mut most = 0.0f64;
            for c in &clean {
                for ch in 0..3 {
                    let i = p * 3 + ch;
                    let d = (f64::from(c.data.get(i).copied().unwrap_or(0))
                        - f64::from(truth.data.get(i).copied().unwrap_or(0)))
                    .abs()
                        / 65535.0;
                    most = most.max(d);
                }
            }
            most
        })
        .collect();
    let rmse = |img: &Image16, keep: &dyn Fn(f64) -> bool| -> (f64, usize) {
        let (mut s, mut n) = (0.0, 0usize);
        for (p, ch) in change.iter().enumerate() {
            if keep(*ch) {
                for c in 0..3 {
                    let i = p * 3 + c;
                    let d = (f64::from(img.data.get(i).copied().unwrap_or(0))
                        - f64::from(truth.data.get(i).copied().unwrap_or(0)))
                        / 65535.0;
                    s += d * d;
                    n += 1;
                }
            }
        }
        ((s / n.max(1) as f64).sqrt(), n / 3)
    };
    let still = |c: f64| c < 0.002;
    let moving = |c: f64| c > 0.01;
    let (s0, ns) = rmse(noisy_mid, &still);
    let (m0, nm) = rmse(noisy_mid, &moving);
    println!(
        "noise σ {sigma}: still {:.1} % of the frame, moving {:.1} %",
        100.0 * ns as f64 / px as f64,
        100.0 * nm as f64 / px as f64
    );
    println!("  noisy frame        still {s0:.5}  moving {m0:.5}");
    let means: Vec<Vec<u16>> = noisy.iter().map(temporal::box_means).collect();
    let profiles: Vec<_> = noisy
        .windows(2)
        .filter_map(|w| match w {
            [a, b] => temporal::estimate_noise(a, b),
            _ => None,
        })
        .collect();
    let profile = temporal::median_profile(&profiles).ok_or("no noise profile")?;
    println!("  measured noise     {:.5} (mid grey)", profile.at(0.5));
    for (radius, tolerance) in [(1usize, 1.0f32), (2, 1.0), (4, 1.0), (2, 2.0)] {
        let lo = 4 - radius;
        let frames: Vec<&Image16> = noisy.get(lo..=4 + radius).ok_or("frames")?.iter().collect();
        let m: Vec<&[u16]> = means
            .get(lo..=4 + radius)
            .ok_or("frames")?
            .iter()
            .map(Vec::as_slice)
            .collect();
        let started = std::time::Instant::now();
        let out = temporal::denoise(
            &frames,
            &m,
            radius,
            &profile,
            &DenoiseParams { radius, tolerance },
        )
        .ok_or("denoise failed")?;
        let t = started.elapsed().as_secs_f64();
        let (s1, _) = rmse(&out, &still);
        let (m1, _) = rmse(&out, &moving);
        println!(
            "  radius {radius} tol {tolerance}  still {s1:.5} ({:.2}× less)  moving {m1:.5} ({:.2}×)  {t:.2} s",
            s0 / s1,
            m0 / m1
        );
    }
    Ok(())
}

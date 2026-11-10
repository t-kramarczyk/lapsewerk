//! Checks the regional deflicker against a known truth, on synthetic sequences in linear light.
//!
//! `cargo run --release -p lapsewerk-core --example regional_check -- [COLSxROWS]`
//!
//! Every frame is a still, textured scene whose light fades slowly (a sunset, 1.5 stops over the
//! sequence) times the flicker of the case:
//!
//! - `lamp`: a street lamp lights the lower left of the frame and flickers there (mains, ±0.12 stops
//!   at random); the whole frame flickers a little too (±0.03, the aperture).
//! - `vignette`: aperture flicker that is stronger at the edges than in the middle (the vignetting
//!   changes from frame to frame): ±0.04 in the centre, ±0.15 in the corners.
//! - `cloud`: no flicker at all, but a bright cloud drifts across the sky: real change the regional
//!   deflicker must leave alone (it smooths each tile over time, so it flattens a passing cloud).
//!
//! Each case runs as the command line does: the global deflicker on the whole frame's brightness,
//! then the tiles measured on the frames as corrected, regional corrections from them, applied in
//! linear light. Reported: the error against the truth (the scene with the sunset only, plus the
//! cloud where there is one), per tile and frame, as RMS in stops after each tile's constant offset
//! is removed, and its frame-to-frame part (RMS of second differences).

use lapsewerk_core::deflicker::{self, DeflickerParams, ImageView, MeasureOptions};
use lapsewerk_core::regional::{self, Grid};

const W: usize = 192;
const H: usize = 128;
const FRAMES: usize = 120;

/// Deterministic uniform numbers in -1..1 (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Case {
    Lamp,
    Vignette,
    Cloud,
}

/// The still scene: a sky gradient over textured ground, linear light.
fn base(x: usize, y: usize) -> f64 {
    let u = x as f64 / W as f64;
    let v = y as f64 / H as f64;
    if v < 0.45 {
        0.30 - 0.15 * v
    } else {
        let tex = ((x * 7 + y * 13) % 17) as f64 / 17.0;
        0.06 + 0.05 * tex + 0.03 * (u * 9.0).sin().abs()
    }
}

/// Frame `t`: the truth (no flicker) and the frame as shot.
fn frame(case: Case, t: usize, flicker: &[(f64, f64)]) -> (Vec<f32>, Vec<f32>) {
    let sunset = -1.5 * t as f64 / (FRAMES - 1) as f64;
    let (a, b) = flicker.get(t).copied().unwrap_or_default();
    let mut truth = Vec::with_capacity(W * H * 3);
    let mut shot = Vec::with_capacity(W * H * 3);
    for y in 0..H {
        for x in 0..W {
            let u = x as f64 / W as f64;
            let v = y as f64 / H as f64;
            let mut light = base(x, y);
            if case == Case::Cloud {
                // A bright cloud, 40 px wide, crossing the sky once.
                let cx = -40.0 + (W as f64 + 80.0) * t as f64 / (FRAMES - 1) as f64;
                let (dx, dy) = (x as f64 - cx, y as f64 - 0.2 * H as f64);
                light += 0.25 * (-(dx * dx + dy * dy) / (2.0 * 20.0 * 20.0)).exp() * (v < 0.45) as u8 as f64;
            }
            let mut stops = sunset;
            let mut lamp = 0.0;
            match case {
                Case::Lamp => {
                    // The lamp's light, strongest at the lower left.
                    let (dx, dy) = (u - 0.15, v - 0.85);
                    lamp = 0.12 * (-(dx * dx + dy * dy) / (2.0 * 0.2 * 0.2)).exp();
                    stops += a;
                }
                Case::Vignette => {
                    let r2 = ((u - 0.5) * (u - 0.5) + (v - 0.5) * (v - 0.5)) / 0.5;
                    stops += a + b * r2;
                }
                Case::Cloud => {}
            }
            let clean = (light * 2f64.powf(sunset) + lamp * 2f64.powf(sunset)) as f32;
            let lamp_flicker = if case == Case::Lamp { 2f64.powf(b) } else { 1.0 };
            let lit = (light * 2f64.powf(stops) + lamp * 2f64.powf(sunset) * lamp_flicker) as f32;
            truth.extend([clean; 3]);
            shot.extend([lit; 3]);
        }
    }
    (truth, shot)
}

fn view(data: &[f32]) -> ImageView<'_> {
    ImageView {
        width: W,
        height: H,
        channels: 3,
        data,
    }
}

/// Error per tile and frame against the truth: (RMS after each tile's offset, frame-to-frame RMS).
fn error(out: &[Vec<Option<f64>>], truth: &[Vec<Option<f64>>]) -> (f64, f64) {
    let tiles = truth.first().map_or(0, Vec::len);
    let (mut level, mut jitter, mut nl, mut nj) = (0.0, 0.0, 0usize, 0usize);
    for k in 0..tiles {
        let d: Vec<f64> = out
            .iter()
            .zip(truth)
            .filter_map(|(o, t)| Some(o.get(k).copied()?? - t.get(k).copied()??))
            .collect();
        let mean = d.iter().sum::<f64>() / d.len().max(1) as f64;
        level += d.iter().map(|e| (e - mean) * (e - mean)).sum::<f64>();
        nl += d.len();
        for w in d.windows(3) {
            if let [p, q, r] = w {
                jitter += (p - 2.0 * q + r).powi(2);
                nj += 1;
            }
        }
    }
    (
        (level / nl.max(1) as f64).sqrt(),
        (jitter / nj.max(1) as f64).sqrt(),
    )
}

fn main() {
    let grid = std::env::args()
        .nth(1)
        .and_then(|g| {
            let (c, r) = g.split_once('x')?;
            Grid::new(c.parse().ok()?, r.parse().ok()?)
        })
        .unwrap_or(Grid { cols: 6, rows: 4 });
    let opts = MeasureOptions::default();
    let params = DeflickerParams::default();
    println!(
        "grid {}x{}, {FRAMES} frames of {W}x{H}; errors against the truth in stops (RMS per tile: level / frame to frame)",
        grid.cols, grid.rows
    );
    for (name, case) in [
        ("lamp", Case::Lamp),
        ("vignette", Case::Vignette),
        ("cloud", Case::Cloud),
    ] {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let flicker: Vec<(f64, f64)> = (0..FRAMES)
            .map(|_| match case {
                Case::Lamp => (0.03 * rng.next(), 0.12 * rng.next()),
                Case::Vignette => (0.04 * rng.next(), 0.11 * rng.next()),
                Case::Cloud => (0.0, 0.0),
            })
            .collect();
        let frames: Vec<(Vec<f32>, Vec<f32>)> = (0..FRAMES).map(|t| frame(case, t, &flicker)).collect();
        let tiles_of = |d: &[f32]| regional::measure_tiles(view(d), grid, &opts);
        let truth: Vec<Vec<Option<f64>>> = frames.iter().map(|(t, _)| tiles_of(t)).collect();

        // As shot.
        let shot_tiles: Vec<Vec<Option<f64>>> = frames.iter().map(|(_, s)| tiles_of(s)).collect();
        // Global deflicker (exposure in linear light is a gain).
        let whole: Vec<Option<f64>> = frames
            .iter()
            .map(|(_, s)| deflicker::measure(view(s), &opts).map(|m| m.stops))
            .collect();
        let c = deflicker::corrections(&whole, &params);
        let global: Vec<Vec<f32>> = frames
            .iter()
            .zip(&c)
            .map(|((_, s), c)| {
                let g = 2f64.powf(*c) as f32;
                s.iter().map(|v| v * g).collect()
            })
            .collect();
        let global_tiles: Vec<Vec<Option<f64>>> = global.iter().map(|f| tiles_of(f)).collect();
        let global_whole: Vec<Option<f64>> = global
            .iter()
            .map(|f| deflicker::measure(view(f), &opts).map(|m| m.stops))
            .collect();
        // Regional on top, measured on the globally corrected frames (the check pass).
        let rc = regional::corrections(&global_tiles, &global_whole, &params);
        let out_tiles: Vec<Vec<Option<f64>>> = global
            .iter()
            .zip(&rc)
            .map(|(f, stops)| {
                let mut f = f.clone();
                regional::apply_linear(&mut f, W, H, grid, stops);
                tiles_of(&f)
            })
            .collect();
        let show = |label: &str, t: &[Vec<Option<f64>>]| {
            let (level, jitter) = error(t, &truth);
            println!("  {label:<18} {level:.4} / {jitter:.4}");
        };
        println!("{name}:");
        show("as shot", &shot_tiles);
        show("global deflicker", &global_tiles);
        show("+ regional", &out_tiles);
    }
}

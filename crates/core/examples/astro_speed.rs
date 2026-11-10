//! Time of the night sky steps on one 3840 × 2160 frame (synthetic sky with 3000 stars).
//!
//! `cargo run --release -p lapsewerk-core --example astro_speed`

#![allow(clippy::expect_used)]

use std::time::Instant;

use lapsewerk_core::astro::{self, StarParams, Trails};
use lapsewerk_core::deflicker::Rect;
use lapsewerk_core::temporal::Image16;

fn main() {
    let (w, h) = (3840usize, 2160usize);
    let mut seed = 0x1234_5678_9abc_def1u64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f32 / (1u64 << 53) as f32
    };
    let enc = |l: f32| (lapsewerk_core::regional::linear_to_srgb(l.clamp(0.0, 1.0)) * 65535.0) as u16;
    let mut lin = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let v = y as f32 / h as f32;
            if let Some(p) = lin.get_mut(y * w + x) {
                *p = 0.01 + 0.05 * v * v + 0.002 * rnd();
            }
        }
    }
    for _ in 0..3000 {
        let (sx, sy, peak) = (rnd() * w as f32, rnd() * h as f32, 0.05 + 0.4 * rnd());
        for dy in -4i32..=4 {
            for dx in -4i32..=4 {
                let (x, y) = (sx as i32 + dx, sy as i32 + dy);
                if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                    let d2 = (x as f32 - sx).powi(2) + (y as f32 - sy).powi(2);
                    if let Some(p) = lin.get_mut(y as usize * w + x as usize) {
                        *p += peak * (-d2 / 1.6).exp();
                    }
                }
            }
        }
    }
    let data: Vec<u16> = lin.iter().flat_map(|l| [enc(*l); 3]).collect();
    let img = Image16::new(w, h, data).expect("size");
    let t = Instant::now();
    let stars = astro::find_stars(&img, &StarParams::default());
    let find = t.elapsed();
    let mut out = img.clone();
    let t = Instant::now();
    astro::reduce_stars(&mut out, &stars, 0.5);
    let reduce = t.elapsed();
    let area = Rect::new(0.0, 0.0, 1.0, 0.6).expect("area");
    let t = Instant::now();
    let g = astro::fit_gradient(&img, area).expect("fit");
    let fit = t.elapsed();
    let t = Instant::now();
    g.apply(&mut out, 1.0);
    let apply = t.elapsed();
    let mut trails = Trails::new(0);
    let t = Instant::now();
    let _ = trails.push(&img);
    let trail = t.elapsed();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    println!(
        "{} stars; find {:.0} ms, reduce {:.0} ms, fit gradient {:.0} ms, apply {:.0} ms, trails {:.0} ms",
        stars.len(),
        ms(find),
        ms(reduce),
        ms(fit),
        ms(apply),
        ms(trail)
    );
}

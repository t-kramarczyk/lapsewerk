//! Dust detection on real frames, with and without planted dust.
//!
//! `cargo run --release -p lapsewerk-core --example dust_check -- DIR [SHRINK] [PLANTED] [OUT]`
//!
//! DIR holds frame dumps `f0.raw`, `f1.raw` … (u32 width, u32 height, then width × height × 3 u16 RGB,
//! sRGB-encoded, little-endian). The frames are shrunk by SHRINK (2) like the map renders, PLANTED
//! spots (0) of known place, size and depth are multiplied into every frame (in linear light), and the
//! spots found are listed, matched against the planted ones, and corrected in one frame to measure what
//! is left of them. With OUT, every frame is written there corrected (same dump format, shrunk).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use lapsewerk_core::dust::{DustParams, Luma, estimate};

fn load(path: &std::path::Path) -> (Vec<u16>, usize, usize) {
    let d = std::fs::read(path).expect("frame dump");
    let w = u32::from_le_bytes(d[0..4].try_into().unwrap()) as usize;
    let h = u32::from_le_bytes(d[4..8].try_into().unwrap()) as usize;
    let px = d[8..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect();
    (px, w, h)
}

/// The image shrunk by `k` (box average), as 16-bit sRGB.
fn shrink(px: &[u16], w: usize, h: usize, k: usize) -> (Vec<u16>, usize, usize) {
    let (sw, sh) = (w / k, h / k);
    let dec = |v: u16| lapsewerk_core::regional::srgb_to_linear(f32::from(v) / 65535.0);
    let mut out = Vec::with_capacity(sw * sh * 3);
    for y in 0..sh {
        for x in 0..sw {
            for c in 0..3 {
                let mut s = 0.0;
                for dy in 0..k {
                    for dx in 0..k {
                        s += dec(px[((y * k + dy) * w + x * k + dx) * 3 + c]);
                    }
                }
                let l = s / (k * k) as f32;
                out.push((lapsewerk_core::regional::linear_to_srgb(l) * 65535.0).round() as u16);
            }
        }
    }
    (out, sw, sh)
}

/// Planted dust: (x, y, radius, depth) in map pixels, spread over the top 40 % of the frame (sky).
fn planted(n: usize, w: usize, h: usize) -> Vec<(f32, f32, f32, f32)> {
    let mut seed = 12345u64;
    let mut rnd = || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (seed >> 40) as f32 / (1u64 << 24) as f32
    };
    (0..n)
        .map(|_| {
            let r = 2.0 + rnd() * 25.0;
            (
                r * 3.0 + rnd() * (w as f32 - 6.0 * r),
                r * 3.0 + rnd() * (h as f32 * 0.4 - 6.0 * r),
                r,
                0.02 + rnd() * 0.06,
            )
        })
        .collect()
}

fn factor(spots: &[(f32, f32, f32, f32)], x: f32, y: f32) -> f32 {
    spots.iter().fold(1.0, |f, &(sx, sy, r, depth)| {
        let d = ((x - sx).powi(2) + (y - sy).powi(2)).sqrt() / r;
        f * (1.0 - depth * (-(d * d) * 2.0).exp())
    })
}

fn plant(px: &mut [u16], w: usize, spots: &[(f32, f32, f32, f32)]) {
    for (i, p) in px.as_chunks_mut::<3>().0.iter_mut().enumerate() {
        let f = factor(spots, (i % w) as f32, (i / w) as f32);
        if f < 0.9999 {
            for v in p.iter_mut() {
                let l = lapsewerk_core::regional::srgb_to_linear(f32::from(*v) / 65535.0) * f;
                *v = (lapsewerk_core::regional::linear_to_srgb(l) * 65535.0).round() as u16;
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::path::PathBuf::from(args.first().expect("DIR"));
    let k: usize = args.get(1).map_or(2, |s| s.parse().unwrap());
    let n_planted: usize = args.get(2).map_or(0, |s| s.parse().unwrap());
    let out = args.get(3).map(std::path::PathBuf::from);
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "raw"))
        .collect();
    paths.sort_by_key(|p| {
        p.file_stem().unwrap().to_string_lossy()[1..]
            .parse::<usize>()
            .unwrap_or(0)
    });

    let mut frames = Vec::new();
    let mut images = Vec::new();
    let mut spots_in = Vec::new();
    for p in &paths {
        let (px, w, h) = load(p);
        let (mut small, sw, sh) = shrink(&px, w, h, k);
        if spots_in.is_empty() && n_planted > 0 {
            spots_in = planted(n_planted, sw, sh);
        }
        plant(&mut small, sw, &spots_in);
        frames.push(Luma::from_srgb16(&small, sw, sh).unwrap());
        images.push((small, sw, sh));
    }
    let started = std::time::Instant::now();
    let map = estimate(&frames, &DustParams::default()).expect("enough frames");
    println!(
        "{} frames {}×{}: {} spots in {:.2} s (pattern noise {:.4})",
        frames.len(),
        map.width,
        map.height,
        map.spots.len(),
        started.elapsed().as_secs_f64(),
        map.noise
    );
    let mut matched = 0;
    for s in &map.spots {
        let hit = spots_in
            .iter()
            .position(|p| ((p.0 - s.x).powi(2) + (p.1 - s.y).powi(2)).sqrt() < p.2.max(3.0));
        if hit.is_some() {
            matched += 1;
        }
        println!(
            "  spot at {:.0},{:.0} (full size {:.0},{:.0}) radius {:.1} depth {:.3}{}",
            s.x,
            s.y,
            s.x * k as f32,
            s.y * k as f32,
            s.radius,
            s.depth,
            hit.map_or(" (not planted)".into(), |i| format!(
                " = planted #{i} depth {:.3}",
                spots_in[i].3
            ))
        );
    }
    if n_planted > 0 {
        let missed: Vec<_> = spots_in
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                !map.spots
                    .iter()
                    .any(|s| ((p.0 - s.x).powi(2) + (p.1 - s.y).powi(2)).sqrt() < p.2.max(3.0))
            })
            .map(|(i, p)| format!("#{i} r {:.1} depth {:.3}", p.2, p.3))
            .collect();
        println!("planted {n_planted}: found {matched}, missed {:?}", missed);
    }
    if let Some(out) = &out {
        std::fs::create_dir_all(out).unwrap();
        for (k, (img, w, h)) in images.iter().enumerate() {
            let mut img = img.clone();
            map.apply_srgb16(&mut img, *w, *h);
            let mut bytes = Vec::with_capacity(8 + img.len() * 2);
            bytes.extend((*w as u32).to_le_bytes());
            bytes.extend((*h as u32).to_le_bytes());
            for v in &img {
                bytes.extend(v.to_le_bytes());
            }
            std::fs::write(out.join(format!("f{k}.raw")), bytes).unwrap();
        }
    }
    // What is left: one frame corrected, against its own surroundings (ratio to the frame without
    // the planted dust for planted spots; the median map for real ones is not available here).
    let (mut img, w, h) = images[images.len() / 2].clone();
    map.apply_srgb16(&mut img, w, h);
    let fixed = Luma::from_srgb16(&img, w, h).unwrap();
    let before = &frames[images.len() / 2];
    if n_planted > 0 {
        let mut worst = 0.0f32;
        for &(x, y, r, depth) in &spots_in {
            let (mut s, mut s0, mut n) = (0.0f32, 0.0f32, 0.0f32);
            for yy in (y - r).max(0.0) as usize..((y + r) as usize).min(h) {
                for xx in (x - r).max(0.0) as usize..((x + r) as usize).min(w) {
                    if ((xx as f32 - x).powi(2) + (yy as f32 - y).powi(2)).sqrt() <= r {
                        let f = factor(&spots_in, xx as f32, yy as f32);
                        let clean = before.data[yy * w + xx] / f;
                        s += fixed.data[yy * w + xx] / clean - 1.0;
                        s0 += before.data[yy * w + xx] / clean - 1.0;
                        n += 1.0;
                    }
                }
            }
            let (left, was) = (s / n, s0 / n);
            worst = worst.max(left.abs());
            println!(
                "  planted r {r:.1} depth {depth:.3}: darkening {:.4} -> {:.4}",
                -was, -left
            );
        }
        println!("worst darkening left: {worst:.4}");
    }
}

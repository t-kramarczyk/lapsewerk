//! The sky of a frame, found by its own light: a region grown from the top edge over smooth, gently
//! changing brightness, measured relative to the frame (so it works at dusk and at night, where
//! the sky is dark but still brighter and smoother than the land), with stars and noise filtered
//! out first. The result is a soft matte (0..255) at the size asked for.
//!
//! LightCraft's own sky shape asks for a sky brighter than a fixed level (−3.5 to −1 stops) and
//! weights the top of the frame: in daylight that works, in the blue hour and at night it selects
//! nothing. A matte from here is given to LightCraft as the source's sky matte, which its sky
//! masks use in place of that heuristic.

/// Working width: the frame is measured at this size (the matte is smooth anyway).
const GRID: usize = 160;

/// A sky matte: `width × height` values 0..255 (255 sky), row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkyMatte {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// Whether two mattes of one size put the sky in different places: more than a twentieth of the
/// frame is sky in one and not in the other (a camera that moved). Of other sizes: `false`.
#[must_use]
pub fn differ(a: &SkyMatte, b: &SkyMatte) -> bool {
    if a.width != b.width || a.height != b.height {
        return false;
    }
    let n = a
        .data
        .iter()
        .zip(&b.data)
        .filter(|(x, y)| (**x > 127) != (**y > 127))
        .count();
    n as f64 > 0.05 * a.data.len().max(1) as f64
}

impl SkyMatte {
    /// The share of the frame that is sky (0..1).
    #[must_use]
    pub fn coverage(&self) -> f64 {
        if self.data.is_empty() {
            return 0.0;
        }
        self.data.iter().map(|&v| f64::from(v)).sum::<f64>() / (255.0 * self.data.len() as f64)
    }
}

/// The median of `v` (reordered); 0 when empty.
fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mid = v.len() / 2;
    v.select_nth_unstable_by(mid, f32::total_cmp);
    v.get(mid).copied().unwrap_or(0.0)
}

/// Finds the sky in a linear-light RGB image (`rgb`: 3 floats a pixel, row by row, the frame
/// upright: sky at the top), and returns its matte at `out_w × out_h`. `None` when the image is
/// too small or its size does not match.
#[must_use]
pub fn find_sky(rgb: &[f32], width: usize, height: usize, out_w: usize, out_h: usize) -> Option<SkyMatte> {
    if width < 8
        || height < 8
        || out_w == 0
        || out_h == 0
        || width.checked_mul(height)?.checked_mul(3)? != rgb.len()
    {
        return None;
    }
    // The grid: box-averaged luminance, in stops.
    let gw = GRID.min(width);
    let gh = ((height as f64 * gw as f64 / width as f64).round() as usize).clamp(8, 4 * GRID);
    let mut sum = vec![0.0f64; gw * gh];
    let mut count = vec![0u32; gw * gh];
    for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
        let (x, y) = (i % width, i / width);
        let (gx, gy) = (x * gw / width, y * gh / height);
        let l = 0.2627 * px[0] + 0.6780 * px[1] + 0.0593 * px[2];
        let l = if l.is_finite() { l.max(0.0) } else { 0.0 };
        if let (Some(s), Some(c)) = (sum.get_mut(gy * gw + gx), count.get_mut(gy * gw + gx)) {
            *s += f64::from(l);
            *c += 1;
        }
    }
    let raw: Vec<f32> = sum
        .iter()
        .zip(&count)
        .map(|(s, &c)| ((s / f64::from(c.max(1))) as f32 + 1e-7).log2())
        .collect();
    // Stars, planes, noise: a 3 × 3 median takes out what is a pixel or two across.
    let at = |v: &[f32], x: isize, y: isize| -> f32 {
        let (x, y) = (
            x.clamp(0, gw as isize - 1) as usize,
            y.clamp(0, gh as isize - 1) as usize,
        );
        v.get(y * gw + x).copied().unwrap_or(0.0)
    };
    let mut l = vec![0.0f32; gw * gh];
    for y in 0..gh {
        for x in 0..gw {
            let mut n = [0.0f32; 9];
            for (k, slot) in n.iter_mut().enumerate() {
                *slot = at(
                    &raw,
                    x as isize + (k % 3) as isize - 1,
                    y as isize + (k / 3) as isize - 1,
                );
            }
            if let Some(v) = l.get_mut(y * gw + x) {
                *v = median(&mut n);
            }
        }
    }
    // How much the brightness changes at each cell (stops): edges stop the sky.
    let grad: Vec<f32> = (0..gw * gh)
        .map(|i| {
            let (x, y) = ((i % gw) as isize, (i / gw) as isize);
            let c = at(&l, x, y);
            [(1, 0), (-1, 0), (0, 1), (0, -1)]
                .iter()
                .map(|(dx, dy)| (at(&l, x + dx, y + dy) - c).abs())
                .fold(0.0, f32::max)
        })
        .collect();
    // The top: what the sky looks like where it surely is (the top rows), and the frame overall.
    let top_rows = (gh / 12).max(1);
    let mut top: Vec<f32> = l.get(..top_rows * gw)?.to_vec();
    let top_level = median(&mut top);
    let mut top_grad: Vec<f32> = grad.get(..top_rows * gw)?.to_vec();
    let calm = median(&mut top_grad);
    let mut all = l.clone();
    let frame_level = median(&mut all);
    // A sky not brighter than the frame (a dark night sky over lit land, fog): grow only over the
    // smooth parts, with a tighter edge.
    let brighter = top_level > frame_level + 0.3;
    let edge = (4.0 * calm).clamp(0.08, if brighter { 0.45 } else { 0.25 });
    let floor = top_level - if brighter { 2.5 } else { 1.0 };
    let step = edge * 0.8;
    let ok =
        |i: usize| -> bool { grad.get(i).is_some_and(|&g| g < edge) && l.get(i).is_some_and(|&v| v > floor) };
    // Grown from the top row.
    let mut sky = vec![false; gw * gh];
    let mut queue: std::collections::VecDeque<usize> = (0..gw).filter(|&i| ok(i)).collect();
    for &i in &queue {
        if let Some(s) = sky.get_mut(i) {
            *s = true;
        }
    }
    while let Some(i) = queue.pop_front() {
        let (x, y) = (i % gw, i / gw);
        let here = l.get(i).copied().unwrap_or(0.0);
        let mut next = [None; 4];
        if x > 0 {
            next[0] = Some(i - 1);
        }
        if x + 1 < gw {
            next[1] = Some(i + 1);
        }
        if y > 0 {
            next[2] = Some(i - gw);
        }
        if y + 1 < gh {
            next[3] = Some(i + gw);
        }
        for j in next.into_iter().flatten() {
            let fresh = sky.get(j).is_some_and(|s| !s);
            let gentle = l.get(j).is_some_and(|v| (v - here).abs() < step);
            if fresh && gentle && ok(j) {
                if let Some(s) = sky.get_mut(j) {
                    *s = true;
                }
                queue.push_back(j);
            }
        }
    }
    // Small holes (a star cluster, a bird, a bright planet) closed: dilate, then erode, by 2 cells.
    let morph = |src: &[bool], grow: bool| -> Vec<bool> {
        (0..gw * gh)
            .map(|i| {
                let (x, y) = ((i % gw) as isize, (i / gw) as isize);
                let mut any = !grow;
                for dy in -2..=2isize {
                    for dx in -2..=2isize {
                        let (nx, ny) = (x + dx, y + dy);
                        // Outside the frame counts as unchanged (edges are not eaten away).
                        let v = if nx < 0 || ny < 0 || nx >= gw as isize || ny >= gh as isize {
                            src.get(i).copied().unwrap_or(false)
                        } else {
                            src.get(ny as usize * gw + nx as usize).copied().unwrap_or(false)
                        };
                        if grow {
                            any |= v;
                        } else {
                            any &= v;
                        }
                    }
                }
                any
            })
            .collect()
    };
    let closed = morph(&morph(&sky, true), false);
    // Soft: box-blurred by a cell either side (the coarse matte).
    let soft: Vec<f32> = (0..gw * gh)
        .map(|i| {
            let (x, y) = ((i % gw) as isize, (i / gw) as isize);
            let mut s = 0.0;
            for dy in -1..=1isize {
                for dx in -1..=1isize {
                    let nx = (x + dx).clamp(0, gw as isize - 1) as usize;
                    let ny = (y + dy).clamp(0, gh as isize - 1) as usize;
                    s += f32::from(u8::from(closed.get(ny * gw + nx).copied().unwrap_or(false)));
                }
            }
            s / 9.0
        })
        .collect();
    // The brightness of sky and of land near each cell (4 cells around): the edge is put where a
    // pixel's own brightness crosses from one to the other, at the image's full resolution, so the
    // matte follows the ridge instead of the grid (no halo of sky colour on the land or back).
    let near = |want: bool| -> Vec<Option<f32>> {
        (0..gw * gh)
            .map(|i| {
                let (x, y) = ((i % gw) as isize, (i / gw) as isize);
                let (mut sum, mut n) = (0.0f32, 0u32);
                for dy in -4..=4isize {
                    for dx in -4..=4isize {
                        let (nx, ny) = (x + dx, y + dy);
                        if nx < 0 || ny < 0 || nx >= gw as isize || ny >= gh as isize {
                            continue;
                        }
                        let k = ny as usize * gw + nx as usize;
                        if closed.get(k).copied() == Some(want)
                            && let Some(v) = l.get(k)
                        {
                            sum += v;
                            n += 1;
                        }
                    }
                }
                (n > 0).then(|| sum / n as f32)
            })
            .collect()
    };
    let (sky_near, land_near) = (near(true), near(false));
    let cell =
        |x: usize, y: usize| -> usize { (y * gh / height).min(gh - 1) * gw + (x * gw / width).min(gw - 1) };
    let coarse = |x: usize, y: usize| -> f32 {
        let fy = ((y as f32 + 0.5) * gh as f32 / height as f32 - 0.5).clamp(0.0, (gh - 1) as f32);
        let fx = ((x as f32 + 0.5) * gw as f32 / width as f32 - 0.5).clamp(0.0, (gw - 1) as f32);
        let (x0, tx, y0, ty) = (fx.floor() as usize, fx.fract(), fy.floor() as usize, fy.fract());
        let (x1, y1) = ((x0 + 1).min(gw - 1), (y0 + 1).min(gh - 1));
        let v = |x: usize, y: usize| soft.get(y * gw + x).copied().unwrap_or(0.0);
        let a = v(x0, y0) + (v(x1, y0) - v(x0, y0)) * tx;
        let b = v(x0, y1) + (v(x1, y1) - v(x0, y1)) * tx;
        a + (b - a) * ty
    };
    let px_l = |x: usize, y: usize| -> f32 {
        let i = (y * width + x) * 3;
        let c = |k: usize| {
            rgb.get(i + k)
                .copied()
                .filter(|v| v.is_finite())
                .unwrap_or(0.0)
                .max(0.0)
        };
        (0.2627 * c(0) + 0.6780 * c(1) + 0.0593 * c(2) + 1e-7).log2()
    };
    let mut full = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let c = coarse(x, y);
            let v = if c <= 0.02 || c >= 0.98 {
                c
            } else {
                let k = cell(x, y);
                match (
                    sky_near.get(k).copied().flatten(),
                    land_near.get(k).copied().flatten(),
                ) {
                    (Some(s), Some(g)) if s - g > 0.4 => {
                        let t = (px_l(x, y) - g) / (s - g);
                        let t = ((t - 0.35) / 0.3).clamp(0.0, 1.0);
                        t * t * (3.0 - 2.0 * t)
                    }
                    _ => c,
                }
            };
            if let Some(f) = full.get_mut(y * width + x) {
                *f = v;
            }
        }
    }
    // At the output size: the mean of the pixels each output pixel covers (bilinear when larger).
    let mut data = Vec::with_capacity(out_w * out_h);
    for oy in 0..out_h {
        let (y0, y1) = (
            oy * height / out_h,
            ((oy + 1) * height / out_h)
                .max(oy * height / out_h + 1)
                .min(height),
        );
        for ox in 0..out_w {
            let (x0, x1) = (
                ox * width / out_w,
                ((ox + 1) * width / out_w).max(ox * width / out_w + 1).min(width),
            );
            let (mut sum, mut n) = (0.0f32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += full.get(y * width + x).copied().unwrap_or(0.0);
                    n += 1;
                }
            }
            data.push((sum / n.max(1) as f32 * 255.0).round().clamp(0.0, 255.0) as u8);
        }
    }
    Some(SkyMatte {
        width: out_w,
        height: out_h,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linear light of `stops` (0 = 1.0).
    fn lin(stops: f32) -> f32 {
        stops.exp2()
    }

    /// A deterministic noise in −1..1.
    fn noise(x: usize, y: usize, seed: u32) -> f32 {
        let mut h = (x as u32).wrapping_mul(0x9e37_79b1) ^ (y as u32).wrapping_mul(0x85eb_ca77) ^ seed;
        h ^= h >> 15;
        h = h.wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 12;
        (h & 0xffff) as f32 / 32768.0 - 1.0
    }

    /// The horizon: a jagged ridge between 45 % and 70 % of the height, with a tree poking up.
    fn horizon(x: usize, w: usize, h: usize) -> usize {
        let u = x as f32 / w as f32;
        let ridge = 0.55 + 0.08 * (u * 9.0).sin() + 0.04 * (u * 23.0).cos();
        let tree = if (0.70..0.73).contains(&u) { 0.2 } else { 0.0 };
        ((ridge - tree) * h as f32) as usize
    }

    /// A scene: sky (`sky` stops at the top, `fall` stops darker at the horizon, colour `tint`)
    /// over land (`land` stops, textured), with stars and lights when given.
    #[allow(clippy::too_many_arguments)]
    fn scene(
        w: usize,
        h: usize,
        sky: f32,
        fall: f32,
        land: f32,
        stars: bool,
        lights: bool,
        clouds: bool,
    ) -> (Vec<f32>, Vec<bool>) {
        let mut rgb = Vec::with_capacity(w * h * 3);
        let mut truth = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let is_sky = y < horizon(x, w, h);
                let (r, g, b) = if is_sky {
                    let mut s = sky - fall * y as f32 / h as f32 + 0.05 * noise(x, y, 1);
                    // Soft clouds: brighter blobs, 30 px across.
                    if clouds {
                        let (u, v) = (x as f32 / 30.0, y as f32 / 18.0);
                        s += 1.2 * ((u.sin() * v.cos()).max(0.0)).powi(2);
                    }
                    let mut v = lin(s);
                    if stars && noise(x, y, 7) > 0.995 {
                        v *= 40.0;
                    }
                    (v * 0.8, v * 0.9, v * 1.2)
                } else {
                    // Rock, grass, trees: strong texture.
                    let s = land + 0.9 * noise(x / 2, y / 2, 3);
                    let mut v = lin(s);
                    if lights && noise(x / 3, y / 3, 11) > 0.97 {
                        v = lin(sky + 2.0);
                    }
                    (v * 1.1, v, v * 0.7)
                };
                rgb.extend_from_slice(&[r, g, b]);
                truth.push(is_sky);
            }
        }
        (rgb, truth)
    }

    /// Intersection over union of the matte (≥ 128) and the truth.
    fn iou(m: &SkyMatte, truth: &[bool]) -> f64 {
        let (mut both, mut either) = (0usize, 0usize);
        for (&v, &t) in m.data.iter().zip(truth) {
            let s = v >= 128;
            both += usize::from(s && t);
            either += usize::from(s || t);
        }
        both as f64 / either.max(1) as f64
    }

    /// LightCraft's sky heuristic (as of its pinned revision), for comparison: bright (−3.5 … −1
    /// stops), blue-ish or bright, weighted to the top. Without its final blur.
    fn lightcraft_heuristic(rgb: &[f32], w: usize, h: usize) -> SkyMatte {
        let smooth = |e0: f32, e1: f32, x: f32| {
            let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let data = rgb
            .chunks(3)
            .enumerate()
            .map(|(i, c)| {
                let y = (i / w) as f32 / h as f32;
                let l = (0.2627 * c[0] + 0.678 * c[1] + 0.0593 * c[2] + 1e-6).log2();
                let blue = (c[2] - c[0]).max(0.0) / (c[2] + 1e-4);
                let top = 1.0 - smooth(0.25, 0.7, y);
                let v = top
                    * smooth(-3.5, -1.0, l)
                    * (0.4 + 0.6 * smooth(0.0, 0.3, blue).max(smooth(0.0, 1.5, l)));
                (v * 255.0).round() as u8
            })
            .collect();
        SkyMatte {
            width: w,
            height: h,
            data,
        }
    }

    #[test]
    fn the_sky_is_found_by_day_at_dusk_and_at_night() {
        let (w, h) = (320, 200);
        // (name, sky at the top, darker towards the horizon, land, stars, lights)
        let cases = [
            ("day", 0.0, 0.8, -2.5, false, false, false),
            ("cloudy day", 0.0, 0.8, -2.5, false, false, true),
            ("dusk", -4.5, 1.5, -8.0, false, false, false),
            ("dusk with clouds", -4.5, 1.5, -8.0, false, false, true),
            ("blue hour", -7.0, 1.0, -10.0, false, true, false),
            ("night", -11.0, 0.5, -13.0, true, true, false),
        ];
        for (name, sky, fall, land, stars, lights, clouds) in cases {
            let (rgb, truth) = scene(w, h, sky, fall, land, stars, lights, clouds);
            let m = find_sky(&rgb, w, h, w, h).unwrap();
            let ours = iou(&m, &truth);
            let theirs = iou(&lightcraft_heuristic(&rgb, w, h), &truth);
            eprintln!("{name}: IoU {ours:.3} (LightCraft's heuristic {theirs:.3})");
            assert!(ours > 0.99, "{name}: {ours}");
            // Never darker or brighter: the same scene a stop up or down gives the same sky.
            let brighter: Vec<f32> = rgb.iter().map(|v| v * 2.0).collect();
            let b = find_sky(&brighter, w, h, w, h).unwrap();
            assert!((b.coverage() - m.coverage()).abs() < 0.02, "{name}");
        }
    }

    #[test]
    fn a_frame_without_sky_gets_little_and_bad_input_none() {
        let (w, h) = (200, 120);
        // Textured ground everywhere (a forest floor).
        let rgb: Vec<f32> = (0..w * h)
            .flat_map(|i| {
                let v = lin(-3.0 + 1.2 * noise((i % w) / 2, (i / w) / 2, 5));
                [v, v, v]
            })
            .collect();
        let m = find_sky(&rgb, w, h, 50, 30).unwrap();
        assert!(m.coverage() < 0.1, "{}", m.coverage());
        assert_eq!((m.width, m.height, m.data.len()), (50, 30, 1500));
        assert!(find_sky(&rgb[3..], w, h, 50, 30).is_none());
        assert!(find_sky(&rgb, w, h, 0, 30).is_none());
        let nan = vec![f32::NAN; 16 * 16 * 3];
        assert!(find_sky(&nan, 16, 16, 16, 16).is_some());
    }
}

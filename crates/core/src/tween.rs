//! Frames in between two frames, along their motion (optical flow): for smoother motion or slow
//! motion, and to fill gaps in the capture.
//!
//! - [`flow`]: where each part of `a` moved to in `b`, coarse to fine: block matching on a
//!   brightness pyramid (at most [`FLOW_WIDTH`] wide), a 3×3 median after each level.
//! - [`between`]: the frame at `t` (0: `a`, 1: `b`): `a` moved forward by `t` of the flow and `b`
//!   back by the rest, mixed by `t`. Where the flows there and back disagree (something appears,
//!   disappears or the flow failed), it falls back to a plain cross-fade, which never tears.

use crate::temporal::Image16;

/// Widest brightness image the flow is measured on.
pub const FLOW_WIDTH: usize = 480;

/// Where each grid cell of one frame moved in the other, in full-frame pixels per grid cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Flow {
    pub width: usize,
    pub height: usize,
    /// Full-frame pixels per grid cell.
    pub scale: f32,
    /// Motion per cell, in grid cells.
    pub u: Vec<f32>,
    pub v: Vec<f32>,
}

impl Flow {
    /// The motion at grid position (`x`, `y`), bilinear, in grid cells.
    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        (
            bilinear(&self.u, self.width, self.height, x, y),
            bilinear(&self.v, self.width, self.height, x, y),
        )
    }
}

/// A brightness plane.
struct Plane {
    w: usize,
    h: usize,
    p: Vec<f32>,
}

impl Plane {
    fn get(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        self.p.get(y * self.w + x).copied().unwrap_or(0.0)
    }

    fn half(&self) -> Plane {
        let (w, h) = ((self.w / 2).max(1), (self.h / 2).max(1));
        let mut p = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (2 * x as isize, 2 * y as isize);
                if let Some(v) = p.get_mut(y * w + x) {
                    *v = 0.25
                        * (self.get(sx, sy)
                            + self.get(sx + 1, sy)
                            + self.get(sx, sy + 1)
                            + self.get(sx + 1, sy + 1));
                }
            }
        }
        Plane { w, h, p }
    }
}

/// `img`'s brightness at a `scale`-times smaller grid (box filtered).
fn luma(img: &Image16, scale: usize) -> Plane {
    let (w, h) = (
        img.width.div_ceil(scale).max(1),
        img.height.div_ceil(scale).max(1),
    );
    let mut p = vec![0.0f32; w * h];
    let mut n = vec![0u32; w * h];
    for y in 0..img.height {
        for x in 0..img.width {
            let i = (y * img.width + x) * 3;
            let Some(&[r, g, b]) = img.data.get(i..i + 3) else {
                continue;
            };
            let l = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
            let j = (y / scale) * w + x / scale;
            if let (Some(s), Some(c)) = (p.get_mut(j), n.get_mut(j)) {
                *s += l / 65535.0;
                *c += 1;
            }
        }
    }
    for (s, c) in p.iter_mut().zip(&n) {
        *s /= (*c).max(1) as f32;
    }
    Plane { w, h, p }
}

fn bilinear(p: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
    if w == 0 || h == 0 {
        return 0.0;
    }
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let g = |x: usize, y: usize| p.get(y * w + x).copied().unwrap_or(0.0);
    (g(x0, y0) * (1.0 - fx) + g(x1, y0) * fx) * (1.0 - fy) + (g(x0, y1) * (1.0 - fx) + g(x1, y1) * fx) * fy
}

/// The 3×3 median of a field.
fn median3(f: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = f.to_vec();
    for y in 0..h {
        for x in 0..w {
            let mut n = [0.0f32; 9];
            let mut k = 0;
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    let xx = (x as isize + dx).clamp(0, w as isize - 1) as usize;
                    let yy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                    if let Some(slot) = n.get_mut(k) {
                        *slot = f.get(yy * w + xx).copied().unwrap_or(0.0);
                    }
                    k += 1;
                }
            }
            n.sort_by(f32::total_cmp);
            if let (Some(o), Some(m)) = (out.get_mut(y * w + x), n.get(4)) {
                *o = *m;
            }
        }
    }
    out
}

/// One level of block matching: `u`/`v` (this level's grid) refined by searching ±`radius`.
fn refine(a: &Plane, b: &Plane, u: &mut [f32], v: &mut [f32], radius: isize) {
    const HALF: isize = 2;
    let (w, h) = (a.w, a.h);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let (gu, gv) = (u.get(i).copied().unwrap_or(0.0), v.get(i).copied().unwrap_or(0.0));
            let (bu, bv) = (gu.round() as isize, gv.round() as isize);
            let cost = |du: isize, dv: isize| {
                let mut s = 0.0f32;
                for py in -HALF..=HALF {
                    for px in -HALF..=HALF {
                        let (ax, ay) = (x as isize + px, y as isize + py);
                        s += (a.get(ax, ay) - b.get(ax + du, ay + dv)).abs();
                    }
                }
                s
            };
            // Prefer no change on flat ground: a candidate must do clearly better.
            let mut best = (cost(bu, bv) * 0.999, bu, bv);
            for dv in -radius..=radius {
                for du in -radius..=radius {
                    if du == 0 && dv == 0 {
                        continue;
                    }
                    let c = cost(bu + du, bv + dv);
                    if c < best.0 {
                        best = (c, bu + du, bv + dv);
                    }
                }
            }
            if let (Some(uu), Some(vv)) = (u.get_mut(i), v.get_mut(i)) {
                *uu = best.1 as f32;
                *vv = best.2 as f32;
            }
        }
    }
}

/// Where the parts of `a` moved to in `b`; `None` for frames of different sizes or empty ones.
#[must_use]
pub fn flow(a: &Image16, b: &Image16) -> Option<Flow> {
    if a.width != b.width || a.height != b.height || a.width == 0 || a.height == 0 {
        return None;
    }
    let scale = a.width.div_ceil(FLOW_WIDTH).max(1);
    let mut pa = vec![luma(a, scale)];
    let mut pb = vec![luma(b, scale)];
    while pa.last().is_some_and(|p| p.w > 40 && p.h > 24) && pa.len() < 6 {
        let (Some(na), Some(nb)) = (pa.last().map(Plane::half), pb.last().map(Plane::half)) else {
            break;
        };
        pa.push(na);
        pb.push(nb);
    }
    let mut u: Vec<f32> = Vec::new();
    let mut v: Vec<f32> = Vec::new();
    let mut prev: Option<(usize, usize)> = None;
    for (level, (la, lb)) in pa.iter().zip(&pb).enumerate().rev() {
        // From the coarser level: twice as many cells, twice the motion.
        let (w, h) = (la.w, la.h);
        let mut nu = vec![0.0f32; w * h];
        let mut nv = vec![0.0f32; w * h];
        if let Some((pw, ph)) = prev {
            for y in 0..h {
                for x in 0..w {
                    let (sx, sy) = (x as f32 / 2.0, y as f32 / 2.0);
                    let i = y * w + x;
                    if let (Some(a), Some(b)) = (nu.get_mut(i), nv.get_mut(i)) {
                        *a = 2.0 * bilinear(&u, pw, ph, sx, sy);
                        *b = 2.0 * bilinear(&v, pw, ph, sx, sy);
                    }
                }
            }
        }
        let radius = if level + 1 == pa.len() { 3 } else { 1 };
        refine(la, lb, &mut nu, &mut nv, radius);
        u = median3(&nu, w, h);
        v = median3(&nv, w, h);
        prev = Some((w, h));
    }
    let (w, h) = prev?;
    Some(Flow {
        width: w,
        height: h,
        scale: scale as f32,
        u,
        v,
    })
}

/// The motion at time `t` on each grid cell: `ab`'s cells moved on by `t` of their motion, `ba`'s
/// back by the rest; holes filled from their neighbours.
fn flow_at(ab: &Flow, ba: &Flow, t: f32) -> Flow {
    let (w, h) = (ab.width, ab.height);
    let mut u = vec![0.0f32; w * h];
    let mut v = vec![0.0f32; w * h];
    // Per cell: the motion's size that landed there (larger wins: what moves is usually in front).
    let mut size = vec![-1.0f32; w * h];
    let mut put = |x: f32, y: f32, du: f32, dv: f32| {
        let (qx, qy) = (x.round(), y.round());
        if qx < 0.0 || qy < 0.0 || qx >= w as f32 || qy >= h as f32 {
            return;
        }
        let q = qy as usize * w + qx as usize;
        let m = du.hypot(dv);
        if let (Some(s), Some(uu), Some(vv)) = (size.get_mut(q), u.get_mut(q), v.get_mut(q))
            && m > *s
        {
            *s = m;
            *uu = du;
            *vv = dv;
        }
    };
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let (du, dv) = (
                ab.u.get(i).copied().unwrap_or(0.0),
                ab.v.get(i).copied().unwrap_or(0.0),
            );
            put(x as f32 + t * du, y as f32 + t * dv, du, dv);
            let (bu, bv) = (
                ba.u.get(i).copied().unwrap_or(0.0),
                ba.v.get(i).copied().unwrap_or(0.0),
            );
            put(x as f32 + (1.0 - t) * bu, y as f32 + (1.0 - t) * bv, -bu, -bv);
        }
    }
    // Holes: the mean of the filled neighbours, a few rounds.
    for _ in 0..8 {
        let before = size.clone();
        let mut any = false;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if before.get(i).is_some_and(|s| *s >= 0.0) {
                    continue;
                }
                let (mut su, mut sv, mut n) = (0.0, 0.0, 0.0);
                for (dx, dy) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1)] {
                    let (xx, yy) = (x as isize + dx, y as isize + dy);
                    if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                        continue;
                    }
                    let j = yy as usize * w + xx as usize;
                    if before.get(j).is_some_and(|s| *s >= 0.0) {
                        su += u.get(j).copied().unwrap_or(0.0);
                        sv += v.get(j).copied().unwrap_or(0.0);
                        n += 1.0;
                    }
                }
                if n > 0.0
                    && let (Some(uu), Some(vv), Some(s)) = (u.get_mut(i), v.get_mut(i), size.get_mut(i))
                {
                    *uu = su / n;
                    *vv = sv / n;
                    *s = 0.0;
                    any = true;
                }
            }
        }
        if !any {
            break;
        }
    }
    Flow {
        width: w,
        height: h,
        scale: ab.scale,
        u,
        v,
    }
}

/// One sample of a 16-bit RGB image at (`x`, `y`), bilinear, edges clamped.
fn sample(img: &Image16, x: f32, y: f32, c: usize) -> f32 {
    let (w, h) = (img.width, img.height);
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let g = |x: usize, y: usize| img.data.get((y * w + x) * 3 + c).map_or(0.0, |v| f32::from(*v));
    (g(x0, y0) * (1.0 - fx) + g(x1, y0) * fx) * (1.0 - fy) + (g(x0, y1) * (1.0 - fx) + g(x1, y1) * fx) * fy
}

/// The frame at `t` (0..1) between `a` and `b`, with their flows there (`ab`) and back (`ba`);
/// `None` for frames of different sizes.
#[must_use]
pub fn between(a: &Image16, b: &Image16, ab: &Flow, ba: &Flow, t: f32) -> Option<Image16> {
    if a.width != b.width || a.height != b.height {
        return None;
    }
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.5 };
    let (w, h) = (a.width, a.height);
    let ft = flow_at(ab, ba, t);
    let ft = &ft;
    let mut data = vec![0u16; w * h * 3];
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 16);
    let rows = h.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (k, part) in data.chunks_mut(rows * w * 3).enumerate() {
            s.spawn(move || {
                for (r, line) in part.chunks_mut(w * 3).enumerate() {
                    let y = k * rows + r;
                    for x in 0..w {
                        let (gx, gy) = (x as f32 / ab.scale, y as f32 / ab.scale);
                        // The motion through this pixel at time t.
                        let (u, v) = ft.at(gx, gy);
                        // Trusted when the part of `a` it comes from goes there and back again.
                        let (sx, sy) = (gx - t * u, gy - t * v);
                        let (au, av) = ab.at(sx, sy);
                        let (bu, bv) = ba.at(sx + au, sy + av);
                        let miss = (au + bu).hypot(av + bv) + (au - u).hypot(av - v);
                        let trust = (-(miss * miss) / 2.0).exp();
                        let (dx, dy) = (u * ab.scale, v * ab.scale);
                        let (fx, fy) = (x as f32, y as f32);
                        for c in 0..3 {
                            let moved = (1.0 - t) * sample(a, fx - t * dx, fy - t * dy, c)
                                + t * sample(b, fx + (1.0 - t) * dx, fy + (1.0 - t) * dy, c);
                            let faded = (1.0 - t) * sample(a, fx, fy, c) + t * sample(b, fx, fy, c);
                            let value = trust * moved + (1.0 - trust) * faded;
                            if let Some(o) = line.get_mut(x * 3 + c) {
                                *o = value.round().clamp(0.0, 65535.0) as u16;
                            }
                        }
                    }
                }
            });
        }
    });
    Image16::new(w, h, data)
}

/// `n` frames evenly between `a` and `b` (none for frames of different sizes).
#[must_use]
pub fn frames_between(a: &Image16, b: &Image16, n: usize) -> Vec<Image16> {
    let (Some(ab), Some(ba)) = (flow(a, b), flow(b, a)) else {
        return Vec::new();
    };
    (1..=n)
        .filter_map(|k| between(a, b, &ab, &ba, k as f32 / (n + 1) as f32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grey frame with a textured bright square at `x0`.
    fn square(x0: usize) -> Image16 {
        let (w, h) = (96, 64);
        let mut data = vec![8000u16; w * h * 3];
        for y in 20..44 {
            for x in x0..x0 + 24 {
                let v = 50_000 + ((x - x0) * 300 + y * 200) as u16 % 8000;
                for c in 0..3 {
                    data[(y * w + x) * 3 + c] = v;
                }
            }
        }
        Image16::new(w, h, data).unwrap()
    }

    /// Mean x of the bright pixels, and how many there are.
    fn bright(img: &Image16) -> (f64, usize) {
        let mut sum = 0.0;
        let mut n = 0;
        for y in 0..img.height {
            for x in 0..img.width {
                if img.data[(y * img.width + x) * 3] > 40_000 {
                    sum += x as f64;
                    n += 1;
                }
            }
        }
        (sum / n.max(1) as f64, n)
    }

    #[test]
    fn finds_the_motion() {
        let f = flow(&square(20), &square(30)).unwrap();
        // Inside the square, it moved 10 px right.
        let (u, v) = f.at(32.0 / f.scale, 32.0 / f.scale);
        assert!((u * f.scale - 10.0).abs() < 1.5 && v.abs() < 1.5, "{u} {v}");
    }

    #[test]
    fn the_frame_between_has_the_square_halfway() {
        let (a, b) = (square(20), square(36));
        let mid = frames_between(&a, &b, 1).pop().unwrap();
        let (x, n) = bright(&mid);
        let (xa, na) = bright(&a);
        // Halfway (8 px on), whole: a cross-fade would show two faint squares instead.
        assert!((x - (xa + 8.0)).abs() < 1.5, "{x} vs {}", xa + 8.0);
        assert!(n as f64 > na as f64 * 0.8, "{n} of {na} bright pixels");
        let three = frames_between(&a, &b, 3);
        assert_eq!(three.len(), 3);
        let xs: Vec<f64> = three.iter().map(|f| bright(f).0).collect();
        assert!(xs[0] < xs[1] && xs[1] < xs[2], "{xs:?}");
    }

    #[test]
    #[ignore = "timing: cargo test --release -p lapsewerk-core tween_4k -- --ignored --nocapture"]
    fn tween_4k() {
        let (w, h) = (3840, 2160);
        let make = |o: usize| {
            let data: Vec<u16> = (0..w * h)
                .flat_map(|i| {
                    let (x, y) = (i % w + o, i / w);
                    let v = (((x / 37) ^ (y / 29)) % 7 * 9000) as u16;
                    [v, v, v]
                })
                .collect();
            Image16::new(w, h, data).unwrap()
        };
        let (a, b) = (make(0), make(12));
        let t = std::time::Instant::now();
        let f = frames_between(&a, &b, 1);
        eprintln!("4K in-between frame: {:?}", t.elapsed());
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn still_frames_stay_and_odd_input_is_refused() {
        let a = square(20);
        let mid = frames_between(&a, &a, 1).pop().unwrap();
        assert_eq!(mid, a);
        let small = Image16::new(4, 4, vec![0; 48]).unwrap();
        assert!(frames_between(&a, &small, 1).is_empty());
        assert!(flow(&small, &a).is_none());
    }
}

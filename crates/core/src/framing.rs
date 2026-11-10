//! Framing and camera moves: which part of the frame becomes the video, frame by frame, and the
//! resampling that turns that window into the video's pixels.
//!
//! - [`View`]: where the window sits (0..1 across the room left in the frame, like a crop position)
//!   and how far it is zoomed in (1: the largest window of the video's shape that fits).
//! - [`Move`]: a view at the start and one at the end, eased in and out; zoom changes at a steady
//!   rate (geometric), so a zoom-in does not seem to slow down or speed up.
//! - [`window`]: a view as a pixel rectangle of a frame.
//! - [`resample`]: a window of a 16-bit RGB frame to the video's size, Lanczos-3 (wider when
//!   shrinking, so nothing aliases), on all cores. It works on the encoded values the stages carry
//!   (sRGB or an HDR signal), as video scalers do.

/// Where the video window sits in the frame and how far it is zoomed in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// 0: the window at the left edge, 1: at the right edge.
    pub x: f64,
    /// 0: at the top, 1: at the bottom.
    pub y: f64,
    /// 1: the largest window that fits; 2: half as wide.
    pub zoom: f64,
}

impl View {
    /// Centred, not zoomed.
    pub const CENTRE: Self = Self {
        x: 0.5,
        y: 0.5,
        zoom: 1.0,
    };

    /// Values in range: position 0..1, zoom 1..[`MAX_ZOOM`] (NaN: the centre, not zoomed).
    #[must_use]
    pub fn clamped(self) -> Self {
        let unit = |v: f64, default: f64| if v.is_finite() { v.clamp(0.0, 1.0) } else { default };
        Self {
            x: unit(self.x, 0.5),
            y: unit(self.y, 0.5),
            zoom: if self.zoom.is_finite() {
                self.zoom.clamp(1.0, MAX_ZOOM)
            } else {
                1.0
            },
        }
    }
}

/// The furthest a move may zoom in (the frame is rendered this much larger).
pub const MAX_ZOOM: f64 = 2.0;

/// A camera move over the whole sequence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Move {
    pub from: View,
    pub to: View,
}

impl Move {
    /// The view at `t` (0: first frame, 1: last), eased in and out.
    #[must_use]
    pub fn at(&self, t: f64) -> View {
        let (a, b) = (self.from.clamped(), self.to.clamped());
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        let e = t * t * (3.0 - 2.0 * t);
        View {
            x: a.x + (b.x - a.x) * e,
            y: a.y + (b.y - a.y) * e,
            zoom: a.zoom * (b.zoom / a.zoom).powf(e),
        }
    }

    /// The view at frame `i` of `n`.
    #[must_use]
    pub fn at_frame(&self, i: usize, n: usize) -> View {
        self.at(if n > 1 { i as f64 / (n - 1) as f64 } else { 0.0 })
    }

    /// The most it zooms in.
    #[must_use]
    pub fn max_zoom(&self) -> f64 {
        self.from.clamped().zoom.max(self.to.clamped().zoom)
    }

    /// Does it move at all?
    #[must_use]
    pub fn moves(&self) -> bool {
        self.from.clamped() != self.to.clamped()
    }
}

/// A window in pixels: left, top, width, height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// `view` as a window of the `aspect` (width / height) in a `frame_w` × `frame_h` frame.
#[must_use]
pub fn window(view: View, frame_w: f64, frame_h: f64, aspect: f64) -> Window {
    let v = view.clamped();
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        1.0
    };
    let (fw, fh) = (frame_w.max(1.0), frame_h.max(1.0));
    let (w0, h0) = if fw / fh > aspect {
        (fh * aspect, fh)
    } else {
        (fw, fw / aspect)
    };
    let (width, height) = (w0 / v.zoom, h0 / v.zoom);
    Window {
        left: (fw - width) * v.x,
        top: (fh - height) * v.y,
        width,
        height,
    }
}

/// The Lanczos-3 weights of one output sample: the first source index and its weights (summing to 1).
fn taps(centre: f64, scale: f64, len: usize) -> (usize, Vec<f32>) {
    // Shrinking: the kernel widens by the factor, so it filters out what the output cannot show.
    let support = 3.0 * scale.max(1.0);
    let first = (centre - support).floor().max(0.0) as usize;
    let last = ((centre + support).ceil() as usize).min(len.saturating_sub(1));
    let lanczos = |x: f64| {
        let x = x.abs();
        if x < 1e-9 {
            1.0
        } else if x >= 3.0 {
            0.0
        } else {
            let px = std::f64::consts::PI * x;
            3.0 * px.sin() * (px / 3.0).sin() / (px * px)
        }
    };
    let mut w: Vec<f64> = (first..=last.max(first))
        .map(|j| lanczos((j as f64 + 0.5 - centre) / scale.max(1.0)))
        .collect();
    let sum: f64 = w.iter().sum();
    if sum.abs() < 1e-12 {
        // Outside the frame: the nearest pixel.
        w = vec![0.0; w.len()];
        let k = ((centre - 0.5).round().max(0.0) as usize).saturating_sub(first);
        let k = k.min(w.len().saturating_sub(1));
        if let Some(v) = w.get_mut(k) {
            *v = 1.0;
        }
    } else {
        for v in &mut w {
            *v /= sum;
        }
    }
    (first, w.into_iter().map(|v| v as f32).collect())
}

/// The `win` of a `src_w` × `src_h` 16-bit RGB image, resampled to `out_w` × `out_h`; `None` for a
/// malformed image or an empty size.
#[must_use]
pub fn resample(
    src: &[u16],
    src_w: usize,
    src_h: usize,
    win: Window,
    out_w: usize,
    out_h: usize,
) -> Option<Vec<u16>> {
    if src_w == 0 || src_h == 0 || out_w == 0 || out_h == 0 || src.len() < src_w * src_h * 3 {
        return None;
    }
    let ok = |v: f64| v.is_finite() && v > 0.0;
    if !ok(win.width) || !ok(win.height) || !win.left.is_finite() || !win.top.is_finite() {
        return None;
    }
    let sx = win.width / out_w as f64;
    let sy = win.height / out_h as f64;
    let xs: Vec<(usize, Vec<f32>)> = (0..out_w)
        .map(|x| taps(win.left + (x as f64 + 0.5) * sx, sx, src_w))
        .collect();
    let ys: Vec<(usize, Vec<f32>)> = (0..out_h)
        .map(|y| taps(win.top + (y as f64 + 0.5) * sy, sy, src_h))
        .collect();
    // Only the source rows the window needs.
    let row0 = ys.iter().map(|(f, _)| *f).min().unwrap_or(0);
    let row1 = ys.iter().map(|(f, w)| f + w.len()).max().unwrap_or(0).min(src_h);
    let rows = row1.saturating_sub(row0);
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 16);

    // Across: source rows → out_w wide, as floats.
    let mut across = vec![0f32; rows * out_w * 3];
    let chunk = rows.div_ceil(threads).max(1) * out_w * 3;
    std::thread::scope(|s| {
        for (c, part) in across.chunks_mut(chunk).enumerate() {
            let xs = &xs;
            s.spawn(move || {
                for (r, line) in part.chunks_mut(out_w * 3).enumerate() {
                    let y = row0 + c * (chunk / (out_w * 3)) + r;
                    let Some(row) = src.get(y * src_w * 3..(y + 1) * src_w * 3) else {
                        continue;
                    };
                    for (x, (first, w)) in xs.iter().enumerate() {
                        let mut acc = [0f32; 3];
                        for (k, wk) in w.iter().enumerate() {
                            let i = (first + k) * 3;
                            if let Some(px) = row.get(i..i + 3) {
                                for (a, p) in acc.iter_mut().zip(px) {
                                    *a += wk * f32::from(*p);
                                }
                            }
                        }
                        if let Some(o) = line.get_mut(x * 3..x * 3 + 3) {
                            o.copy_from_slice(&acc);
                        }
                    }
                }
            });
        }
    });

    // Down: out_h rows from those.
    let mut out = vec![0u16; out_w * out_h * 3];
    let chunk = out_h.div_ceil(threads).max(1) * out_w * 3;
    std::thread::scope(|s| {
        for (c, part) in out.chunks_mut(chunk).enumerate() {
            let (ys, across) = (&ys, &across);
            s.spawn(move || {
                for (r, line) in part.chunks_mut(out_w * 3).enumerate() {
                    let Some((first, w)) = ys.get(c * (chunk / (out_w * 3)) + r) else {
                        continue;
                    };
                    for (x, o) in line.iter_mut().enumerate() {
                        let mut acc = 0f32;
                        for (k, wk) in w.iter().enumerate() {
                            let row = (first + k).saturating_sub(row0);
                            if let Some(v) = across.get(row * out_w * 3 + x) {
                                acc += wk * v;
                            }
                        }
                        *o = acc.round().clamp(0.0, 65535.0) as u16;
                    }
                }
            });
        }
    });
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_ease_and_zoom_steadily() {
        let m = Move {
            from: View::CENTRE,
            to: View {
                x: 1.0,
                y: 0.5,
                zoom: 2.0,
            },
        };
        assert_eq!(m.at(0.0), View::CENTRE);
        assert_eq!(m.at_frame(9, 10).zoom, 2.0);
        let mid = m.at(0.5);
        assert!((mid.zoom - 2f64.sqrt()).abs() < 1e-12, "{mid:?}");
        assert!((mid.x - 0.75).abs() < 1e-12);
        // Eased: slow at the ends.
        assert!(m.at(0.05).x - 0.5 < 0.05 * 0.5);
        assert_eq!(m.max_zoom(), 2.0);
        assert!(m.moves());
        assert!(
            !Move {
                from: View::CENTRE,
                to: View::CENTRE
            }
            .moves()
        );
        // Hostile values are reined in.
        let wild = View {
            x: f64::NAN,
            y: 7.0,
            zoom: 99.0,
        }
        .clamped();
        assert_eq!((wild.x, wild.y, wild.zoom), (0.5, 1.0, MAX_ZOOM));
    }

    #[test]
    fn windows_keep_the_shape_and_stay_inside() {
        // A 3:2 frame, a 16:9 video: full width, cropped top and bottom.
        let w = window(View::CENTRE, 6000.0, 4000.0, 16.0 / 9.0);
        assert_eq!(w.width, 6000.0);
        assert!((w.height - 3375.0).abs() < 1e-9);
        assert!((w.top - 312.5).abs() < 1e-9);
        // Zoomed in twice at the bottom right.
        let w = window(
            View {
                x: 1.0,
                y: 1.0,
                zoom: 2.0,
            },
            6000.0,
            4000.0,
            16.0 / 9.0,
        );
        assert!((w.left + w.width - 6000.0).abs() < 1e-9 && (w.top + w.height - 4000.0).abs() < 1e-9);
        assert_eq!(w.width, 3000.0);
    }

    #[test]
    fn resampling_keeps_flat_areas_and_moves_edges() {
        // A flat grey stays exactly that, shrinking or enlarging.
        let src = vec![30_000u16; 40 * 20 * 3];
        let win = window(View::CENTRE, 40.0, 20.0, 2.0);
        for (w, h) in [(10, 5), (80, 40), (33, 17)] {
            let out = resample(&src, 40, 20, win, w, h).unwrap();
            assert_eq!(out.len(), w * h * 3);
            assert!(out.iter().all(|v| *v == 30_000), "{w}×{h}");
        }
        // A vertical edge at x = 20 lands mid-frame at the same size; the right half of the window
        // puts it at the left edge.
        let src: Vec<u16> = (0..20 * 40)
            .flat_map(|i| {
                let v = if i % 40 < 20 { 0 } else { 60_000 };
                [v, v, v]
            })
            .collect();
        let out = resample(&src, 40, 20, win, 40, 20).unwrap();
        let px = |o: &[u16], x: usize| o[(10 * 40 + x) * 3];
        assert!(px(&out, 17) < 2000 && px(&out, 22) > 58_000);
        let half = Window {
            left: 20.0,
            top: 0.0,
            width: 20.0,
            height: 20.0,
        };
        let out = resample(&src, 40, 20, half, 20, 20).unwrap();
        assert!(out.iter().all(|v| *v > 55_000), "{:?}", &out[..12]);
        // Malformed input.
        assert!(resample(&src, 40, 20, win, 0, 5).is_none());
        assert!(resample(&src[..10], 40, 20, win, 4, 4).is_none());
        let bad = Window {
            width: f64::NAN,
            ..win
        };
        assert!(resample(&src, 40, 20, bad, 4, 4).is_none());
    }
}

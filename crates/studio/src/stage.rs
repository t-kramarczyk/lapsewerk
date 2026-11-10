//! The frame stage between rendering and encoding: sliding windows over the rendered frames for the
//! processing that needs neighbouring frames, in this order:
//!
//! 0. repairs ([`Repair`]): odd frames replaced by their neighbours blended, frames put into gaps;
//! 1. temporal noise reduction ([`Denoise`]): `radius` frames on each side;
//! 2. light pollution taken out ([`Gradient`]): fitted per frame, averaged over neighbours;
//! 3. stars reduced ([`lapsewerk_core::astro::reduce_stars`]), frame by frame;
//! 4. star trails ([`lapsewerk_core::astro::Trails`]);
//!
//! 2 to 4 work in the sky found in the first frame ([`astro::sky_matte`]), down to a horizon that
//! is not straight; where none is found, in the sky area given (`--sky`, the app's sky share).
//! Star trails keep to the sky found; where it moves in the frame (a panning camera), to the area
//! given too.
//! 5. frame blending ([`Blend`]): motion blur over `count` frames.
//!
//! Frames go in in order and come out in order, each window delaying them by the frames it looks
//! ahead; a window holds only the frames it needs.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use lapsewerk_core::astro::{self, SkyGradient, StarParams, Trails};
use lapsewerk_core::deflicker::Rect;
use lapsewerk_core::skymask::SkyMatte;
use lapsewerk_core::temporal::{self, DenoiseParams, Image16, NoiseProfile};

/// The night sky work of the stage.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AstroParams {
    /// Light pollution: the sky area (of the video frame) and how much to take out (0..1).
    pub light_pollution: Option<(Rect, f32)>,
    /// Fraction of the stars' light taken out (0..1).
    pub reduce_stars: Option<f32>,
    /// Star trails fading over this many frames (0: endless).
    pub trails: Option<usize>,
    /// Where trails form (`--sky`); `None`: the whole frame.
    pub trails_area: Option<Rect>,
}

/// Frames to replace and gaps to fill.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepairParams {
    /// Frames replaced by their neighbours blended (a flash, headlights, a bird).
    pub replace: Vec<usize>,
    /// (before which frame, how many): frames put in, blended between the two around the gap.
    pub fill: Vec<(usize, usize)>,
}

impl RepairParams {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.replace.is_empty() && self.fill.is_empty()
    }
}

pub struct Stage {
    /// The sky found in the first frame, for the night sky work (`None` inside: none found);
    /// `None`: not looked for yet.
    sky: Option<Option<SkyMatte>>,
    /// The sky found again in a later frame lies elsewhere (a panning or tracked camera): the
    /// first frame's sky no longer fits the foreground.
    sky_moved: bool,
    /// Later frames the sky was looked for again in (a few, spread).
    sky_checks: usize,
    repair: Option<Repair>,
    denoise: Option<Denoise>,
    gradient: Option<Gradient>,
    reduce_stars: Option<f32>,
    trails: Option<Trails>,
    blend: Option<Blend>,
    tween: Option<Tween>,
    /// Stars reduced, over all frames.
    pub stars: usize,
    /// Frames whose light pollution could not be fitted (left as they were).
    pub unfitted: usize,
    /// Time spent in the stage.
    pub time: Duration,
}

impl Stage {
    /// A stage with temporal noise reduction (`denoise`) and frame blending over `blend` frames
    /// (1: none). Without either, frames pass straight through.
    pub fn new(
        denoise: Option<DenoiseParams>,
        blend: usize,
        astro: AstroParams,
        repair: &RepairParams,
        tween: usize,
    ) -> Self {
        Self {
            sky: None,
            sky_moved: false,
            sky_checks: 0,
            repair: (!repair.is_empty()).then(|| Repair::new(repair)),
            denoise: denoise.filter(|d| d.radius > 0).map(Denoise::new),
            gradient: astro
                .light_pollution
                .map(|(area, amount)| Gradient::new(area, amount)),
            reduce_stars: astro.reduce_stars.filter(|a| *a > 0.0),
            trails: astro.trails.map(|n| {
                let t = Trails::new(n);
                match astro.trails_area {
                    Some(area) => t.in_area(area),
                    None => t,
                }
            }),
            blend: (blend > 1).then(|| Blend::new(blend)),
            tween: (tween > 0).then_some(Tween { n: tween, prev: None }),
            stars: 0,
            unfitted: 0,
            time: Duration::ZERO,
        }
    }

    /// Whether the night sky work is asked for (it then wants a sky: [`Self::set_sky`]).
    #[must_use]
    pub fn wants_sky(&self) -> bool {
        self.gradient.is_some() || self.reduce_stars.is_some() || self.trails.is_some()
    }

    /// The sky for the night sky work, found beforehand (in the first frame, or a later one where
    /// the first shows none: city light over a dark land); not looked for in the first frame then.
    pub fn set_sky(&mut self, sky: Option<SkyMatte>) {
        if let (Some(m), Some(t)) = (&sky, &mut self.trails) {
            t.set_sky(m.clone());
        }
        if let (Some(m), Some(g)) = (&sky, &mut self.gradient) {
            g.sky = Some(m.clone());
        }
        self.sky = Some(sky);
    }

    /// Says whether the sky moves in the frame, as found before the render
    /// ([`crate::export::sky_moves`]): with a sky found that stays, trails form in all of it (the
    /// area given is for a sky not found); with one that moves, they keep within the area too, so
    /// foreground lights moving into the first frame's sky leave none.
    pub fn set_sky_moving(&mut self, moving: bool) {
        self.sky_moved = moving;
        if !moving
            && matches!(&self.sky, Some(Some(_)))
            && let Some(t) = &mut self.trails
        {
            t.set_area(None);
        }
    }

    /// Whether the sky found again in a later frame lies elsewhere than in the first (a camera
    /// that pans or tracks the stars).
    #[must_use]
    pub fn sky_moved(&self) -> bool {
        self.sky_moved
    }

    /// The share of the frame the night sky work found to be sky (0..1); `None` when none was found
    /// (it then works in the area given) or it was not looked for.
    #[must_use]
    pub fn sky_share(&self) -> Option<f64> {
        self.sky.as_ref()?.as_ref().map(SkyMatte::coverage)
    }

    /// Takes frame `index` (frames must come in order) and returns the frames now ready, in order.
    pub fn push(&mut self, index: usize, image: Image16) -> Vec<Image16> {
        let started = Instant::now();
        // The sky, once, from the first frame (the night sky work only).
        if self.sky.is_none() && self.wants_sky() {
            self.set_sky(astro::sky_matte(&image));
        }
        // Every 40 frames (8 times at most), the sky looked for again: where it lies elsewhere by
        // more than a twentieth of the frame, the camera moves and is said to (the trails keep the
        // first frame's sky; `--sky` keeps the foreground out).
        if index > 0
            && index.is_multiple_of(40)
            && self.sky_checks < 8
            && !self.sky_moved
            && self.trails.is_some()
            && let Some(Some(first)) = &self.sky
        {
            self.sky_checks += 1;
            if let Some(now) = astro::sky_matte(&image) {
                self.sky_moved = lapsewerk_core::skymask::differ(first, &now);
            }
        }
        let repaired = match &mut self.repair {
            Some(r) => r.push(index, image),
            None => vec![(index, image)],
        };
        let mut denoised = Vec::new();
        for (i, img) in repaired {
            match &mut self.denoise {
                Some(d) => denoised.extend(d.push(i, img)),
                None => denoised.push((i, img)),
            }
        }
        let out = self.pass_on(denoised);
        let out = self.tweened(out);
        self.time += started.elapsed();
        out
    }

    /// In-between frames before each frame, when asked for.
    fn tweened(&mut self, frames: Vec<Image16>) -> Vec<Image16> {
        match &mut self.tween {
            Some(t) => frames.into_iter().flat_map(|f| t.push(f)).collect(),
            None => frames,
        }
    }

    /// The frames still held, once the last frame is in.
    pub fn finish(&mut self) -> Vec<Image16> {
        let started = Instant::now();
        let mut rest = Vec::new();
        for (i, img) in self.repair.as_mut().map(Repair::finish).unwrap_or_default() {
            match &mut self.denoise {
                Some(d) => rest.extend(d.push(i, img)),
                None => rest.push((i, img)),
            }
        }
        rest.extend(self.denoise.as_mut().map(Denoise::finish).unwrap_or_default());
        let mut out = self.pass_on(rest);
        if let Some(g) = &mut self.gradient {
            let rest = g.finish();
            self.unfitted = g.unfitted;
            out.extend(self.after_gradient(rest));
        }
        if let Some(b) = &mut self.blend {
            out.extend(b.finish());
        }
        let out = self.tweened(out);
        self.time += started.elapsed();
        out
    }

    /// Frames out of the noise reduction, through the rest of the stage.
    fn pass_on(&mut self, frames: Vec<(usize, Image16)>) -> Vec<Image16> {
        let frames = match &mut self.gradient {
            Some(g) => {
                let out: Vec<(usize, Image16)> = frames.into_iter().flat_map(|(i, f)| g.push(i, f)).collect();
                self.unfitted = g.unfitted;
                out
            }
            None => frames,
        };
        self.after_gradient(frames)
    }

    /// Stars, trails and blending, frame by frame.
    fn after_gradient(&mut self, frames: Vec<(usize, Image16)>) -> Vec<Image16> {
        let frames: Vec<(usize, Image16)> = frames
            .into_iter()
            .map(|(i, mut f)| {
                if let Some(amount) = self.reduce_stars {
                    let mut stars = astro::find_stars(&f, &StarParams::default());
                    // Only stars in the sky: lamps and lit windows on the land stay as they are.
                    if let Some(Some(m)) = &self.sky
                        && m.width == f.width
                        && m.height == f.height
                    {
                        stars.retain(|s| {
                            astro::sky_at(m, s.x.round().max(0.0) as usize, s.y.round().max(0.0) as usize)
                                >= 0.5
                        });
                    }
                    self.stars += astro::reduce_stars(&mut f, &stars, amount);
                }
                if let Some(t) = &mut self.trails {
                    f = t.push(&f);
                }
                (i, f)
            })
            .collect();
        self.pass_to_blend(frames)
    }

    fn pass_to_blend(&mut self, frames: Vec<(usize, Image16)>) -> Vec<Image16> {
        match &mut self.blend {
            Some(b) => frames.into_iter().flat_map(|(i, f)| b.push(i, f)).collect(),
            None => frames.into_iter().map(|(_, f)| f).collect(),
        }
    }
}

/// In-between frames along the motion: `n` before every frame but the first.
struct Tween {
    n: usize,
    prev: Option<Image16>,
}

impl Tween {
    fn push(&mut self, f: Image16) -> Vec<Image16> {
        let mut out = match &self.prev {
            Some(p) => lapsewerk_core::tween::frames_between(p, &f, self.n),
            None => Vec::new(),
        };
        self.prev = Some(f.clone());
        out.push(f);
        out
    }
}

/// Repairs: odd frames replaced, gaps filled. Frames come out numbered anew (gaps add frames).
struct Repair {
    replace: HashSet<usize>,
    fill: HashMap<usize, usize>,
    /// The last good frame.
    prev: Option<Image16>,
    /// Odd frames waiting for the next good one.
    held: usize,
    next_out: usize,
}

impl Repair {
    fn new(p: &RepairParams) -> Self {
        Self {
            replace: p.replace.iter().copied().collect(),
            fill: p.fill.iter().copied().collect(),
            prev: None,
            held: 0,
            next_out: 0,
        }
    }

    fn emit(&mut self, out: &mut Vec<(usize, Image16)>, img: Image16) {
        out.push((self.next_out, img));
        self.next_out += 1;
    }

    /// `m` frames between `a` and `b`: along their motion (optical flow), else blended in linear
    /// light (`k / (m + 1)` of the way).
    fn between(a: Option<&Image16>, b: &Image16, m: usize) -> Vec<Image16> {
        if let Some(a) = a {
            let moved = lapsewerk_core::tween::frames_between(a, b, m);
            if moved.len() == m {
                return moved;
            }
        }
        (1..=m)
            .map(|k| {
                let w = k as f32 / (m + 1) as f32;
                a.and_then(|a| temporal::blend(&[a, b], &[1.0 - w, w]))
                    .unwrap_or_else(|| b.clone())
            })
            .collect()
    }

    fn push(&mut self, index: usize, image: Image16) -> Vec<(usize, Image16)> {
        let mut out = Vec::new();
        if self.replace.contains(&index) {
            self.held += 1;
            return out;
        }
        // Odd frames before this one: blended from the good frames on either side.
        let held = std::mem::take(&mut self.held);
        for f in Self::between(self.prev.as_ref(), &image, held) {
            self.emit(&mut out, f);
        }
        if let Some(&m) = self.fill.get(&index)
            && self.prev.is_some()
        {
            for f in Self::between(self.prev.as_ref(), &image, m) {
                self.emit(&mut out, f);
            }
        }
        self.prev = Some(image.clone());
        self.emit(&mut out, image);
        out
    }

    fn finish(&mut self) -> Vec<(usize, Image16)> {
        let mut out = Vec::new();
        let held = std::mem::take(&mut self.held);
        if let Some(last) = self.prev.clone() {
            for _ in 0..held {
                self.emit(&mut out, last.clone());
            }
        }
        out
    }
}

/// A window of frames by sequence index.
struct Window<T> {
    slots: VecDeque<(usize, T)>,
    next_out: usize,
}

impl<T> Window<T> {
    fn new() -> Self {
        Self {
            slots: VecDeque::new(),
            next_out: 0,
        }
    }

    /// The frames from `lo` to `hi` (inclusive) that are held, and the position of `center` among them.
    fn around(&self, lo: usize, hi: usize, center: usize) -> (Vec<&T>, Option<usize>) {
        let held: Vec<&(usize, T)> = self.slots.iter().filter(|(i, _)| (lo..=hi).contains(i)).collect();
        let pos = held.iter().position(|(i, _)| *i == center);
        (held.into_iter().map(|(_, t)| t).collect(), pos)
    }

    /// Forgets frames before `first`.
    fn drop_before(&mut self, first: usize) {
        while self.slots.front().is_some_and(|(i, _)| *i < first) {
            self.slots.pop_front();
        }
    }

    fn holds_from(&self, index: usize) -> bool {
        self.slots.iter().any(|(i, _)| *i >= index)
    }
}

struct DenoiseSlot {
    image: Image16,
    means: Vec<u16>,
    /// Noise measured between this frame and the one before it.
    pair: Option<NoiseProfile>,
}

/// Temporal noise reduction over `radius` frames on each side ([`temporal::denoise`]).
struct Denoise {
    params: DenoiseParams,
    window: Window<DenoiseSlot>,
}

impl Denoise {
    fn new(params: DenoiseParams) -> Self {
        Self {
            params,
            window: Window::new(),
        }
    }

    fn push(&mut self, index: usize, image: Image16) -> Vec<(usize, Image16)> {
        let means = temporal::box_means(&image);
        let pair = self
            .window
            .slots
            .back()
            .filter(|(i, _)| i + 1 == index)
            .and_then(|(_, prev)| temporal::estimate_noise(&prev.image, &image));
        self.window
            .slots
            .push_back((index, DenoiseSlot { image, means, pair }));
        let mut out = Vec::new();
        while self.window.next_out + self.params.radius <= index {
            out.extend(self.emit());
        }
        out
    }

    fn finish(&mut self) -> Vec<(usize, Image16)> {
        let mut out = Vec::new();
        while self.window.holds_from(self.window.next_out) {
            out.extend(self.emit());
        }
        out
    }

    fn emit(&mut self) -> Option<(usize, Image16)> {
        let t = self.window.next_out;
        self.window.next_out += 1;
        let r = self.params.radius;
        let (slots, center) = self.window.around(t.saturating_sub(r), t + r, t);
        let center = center?;
        let pairs: Vec<NoiseProfile> = slots.iter().filter_map(|s| s.pair).collect();
        let result = match temporal::median_profile(&pairs) {
            Some(noise) => {
                let frames: Vec<&Image16> = slots.iter().map(|s| &s.image).collect();
                let means: Vec<&[u16]> = slots.iter().map(|s| s.means.as_slice()).collect();
                temporal::denoise(&frames, &means, center, &noise, &self.params)
            }
            // One frame alone: nothing to average with.
            None => slots.get(center).map(|s| s.image.clone()),
        };
        self.window.drop_before((t + 1).saturating_sub(r));
        result.map(|f| (t, f))
    }
}

/// Frames whose light pollution fits are averaged with frame t's (Gaussian, σ 3 frames).
const GRADIENT_RADIUS: usize = 6;

/// Light pollution taken out: each frame's fit ([`astro::fit_gradient`]) averaged with its
/// neighbours' (the correction follows the moon rising or the dusk, not each frame's noise).
struct Gradient {
    area: Rect,
    amount: f32,
    /// The sky the correction reaches into, in place of the area's soft edge.
    sky: Option<SkyMatte>,
    window: Window<(Option<Image16>, Option<SkyGradient>)>,
    unfitted: usize,
}

impl Gradient {
    fn new(area: Rect, amount: f32) -> Self {
        Self {
            area,
            amount,
            sky: None,
            window: Window::new(),
            unfitted: 0,
        }
    }

    fn push(&mut self, index: usize, image: Image16) -> Vec<(usize, Image16)> {
        let fit = astro::fit_gradient(&image, self.area);
        self.window.slots.push_back((index, (Some(image), fit)));
        let mut out = Vec::new();
        while self.window.next_out + GRADIENT_RADIUS <= index {
            out.extend(self.emit());
        }
        out
    }

    fn finish(&mut self) -> Vec<(usize, Image16)> {
        let mut out = Vec::new();
        while self.window.holds_from(self.window.next_out) {
            out.extend(self.emit());
        }
        out
    }

    fn emit(&mut self) -> Option<(usize, Image16)> {
        let t = self.window.next_out;
        self.window.next_out += 1;
        let fits: Vec<(SkyGradient, f64)> = self
            .window
            .slots
            .iter()
            .filter(|(i, _)| i.abs_diff(t) <= GRADIENT_RADIUS)
            .filter_map(|(i, (_, fit))| {
                let d = i.abs_diff(t) as f64;
                fit.map(|g| (g, (-d * d / 18.0).exp()))
            })
            .collect();
        let mut image = self
            .window
            .slots
            .iter_mut()
            .find(|(i, _)| *i == t)
            .and_then(|(_, (img, _))| img.take());
        if let Some(img) = image.as_mut() {
            match SkyGradient::mean(&fits) {
                Some(g) => g.apply_in(img, self.amount, self.sky.as_ref()),
                None => self.unfitted += 1,
            }
        }
        self.window.drop_before((t + 1).saturating_sub(GRADIENT_RADIUS));
        image.map(|f| (t, f))
    }
}

/// Frame blending over `count` frames ([`temporal::blend`]): frame t is the blend of frames
/// t − (count − 1)/2 … t + count/2, with triangle weights; at the ends of the sequence, of those there.
struct Blend {
    count: usize,
    window: Window<Image16>,
}

impl Blend {
    fn new(count: usize) -> Self {
        Self {
            count,
            window: Window::new(),
        }
    }

    fn behind(&self) -> usize {
        (self.count - 1) / 2
    }

    fn ahead(&self) -> usize {
        self.count / 2
    }

    fn push(&mut self, index: usize, image: Image16) -> Vec<Image16> {
        self.window.slots.push_back((index, image));
        let mut out = Vec::new();
        while self.window.next_out + self.ahead() <= index {
            out.extend(self.emit());
        }
        out
    }

    fn finish(&mut self) -> Vec<Image16> {
        let mut out = Vec::new();
        while self.window.holds_from(self.window.next_out) {
            out.extend(self.emit());
        }
        out
    }

    fn emit(&mut self) -> Option<Image16> {
        let t = self.window.next_out;
        self.window.next_out += 1;
        let lo = t.saturating_sub(self.behind());
        let (frames, center) = self.window.around(lo, t + self.ahead(), t);
        center?;
        // Weights of the full window, cut to the frames there are (the sequence's ends).
        let all = temporal::blend_weights(self.count);
        let first = self.behind() - (t - lo);
        let weights: Vec<f32> = all.iter().skip(first).take(frames.len()).copied().collect();
        let result = temporal::blend(&frames, &weights);
        self.window.drop_before((t + 1).saturating_sub(self.behind()));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(v: u16) -> Image16 {
        Image16::new(2, 2, vec![v; 12]).unwrap()
    }

    #[test]
    fn passes_frames_through_in_order() {
        let mut s = Stage::new(None, 1, AstroParams::default(), &RepairParams::default(), 0);
        let out: Vec<u16> = (0..5u16)
            .flat_map(|i| s.push(usize::from(i), flat(i * 1000)))
            .map(|f| f.data[0])
            .collect();
        assert_eq!(out, vec![0, 1000, 2000, 3000, 4000]);
        assert!(s.finish().is_empty());
    }

    #[test]
    fn every_frame_comes_out_once_with_both_windows() {
        let params = DenoiseParams {
            radius: 2,
            tolerance: 1.0,
        };
        for blend in [1, 2, 3, 4] {
            let mut s = Stage::new(
                Some(params),
                blend,
                AstroParams::default(),
                &RepairParams::default(),
                0,
            );
            let mut n = 0;
            for i in 0..9 {
                n += s.push(i, flat(20_000)).len();
            }
            n += s.finish().len();
            assert_eq!(n, 9, "blend {blend}");
        }
    }

    #[test]
    fn in_between_frames_are_added() {
        let mut s = Stage::new(None, 1, AstroParams::default(), &RepairParams::default(), 2);
        let mut n = 0;
        for i in 0..4 {
            n += s.push(i, flat(20_000 + 1000 * i as u16)).len();
        }
        n += s.finish().len();
        // Two between each pair: 4 + 3 × 2.
        assert_eq!(n, 10);
    }

    #[test]
    fn every_frame_comes_out_once_with_the_night_sky_work() {
        let astro = AstroParams {
            light_pollution: Rect::new(0.0, 0.0, 1.0, 1.0).map(|r| (r, 1.0)),
            reduce_stars: Some(0.5),
            trails: Some(0),
            trails_area: None,
        };
        let big = |v: u16| Image16::new(64, 48, vec![v; 64 * 48 * 3]).unwrap();
        for blend in [1, 3] {
            let mut s = Stage::new(None, blend, astro, &RepairParams::default(), 0);
            let mut out = Vec::new();
            for i in 0..15 {
                out.extend(s.push(i, big(9000 + i as u16 * 10)));
            }
            out.extend(s.finish());
            assert_eq!(out.len(), 15, "blend {blend}");
            assert_eq!(s.unfitted, 0);
        }
    }

    #[test]
    fn odd_frames_are_replaced_and_gaps_filled() {
        // Frames 0..6 at 10 000 steps; frame 2 is a flash; two frames are missing before frame 4.
        let repair = RepairParams {
            replace: vec![2],
            fill: vec![(4, 2)],
        };
        let mut s = Stage::new(None, 1, AstroParams::default(), &repair, 0);
        let mut out = Vec::new();
        for i in 0..6usize {
            let v = if i == 2 { 65_000 } else { 10_000 * i as u16 };
            out.extend(s.push(i, flat(v)));
        }
        out.extend(s.finish());
        let first: Vec<u16> = out.iter().map(|f| f.data[0]).collect();
        // 6 frames, the flash replaced by a blend of 1 and 3, two put in between 3 and 4.
        assert_eq!(first.len(), 8);
        assert!(first.iter().all(|&v| v < 60_000), "the flash is gone: {first:?}");
        assert!(first[2] > first[1] && first[2] < first[3], "blended: {first:?}");
        assert!(
            first[4] > first[3] && first[5] > first[4] && first[5] < first[6],
            "filled: {first:?}"
        );
    }

    #[test]
    fn blending_a_steady_scene_changes_nothing() {
        let mut s = Stage::new(None, 3, AstroParams::default(), &RepairParams::default(), 0);
        let mut out = Vec::new();
        for i in 0..6 {
            out.extend(s.push(i, flat(30_000)));
        }
        out.extend(s.finish());
        assert_eq!(out.len(), 6);
        assert!(out.iter().all(|f| f.data.iter().all(|v| v.abs_diff(30_000) <= 1)));
    }
}

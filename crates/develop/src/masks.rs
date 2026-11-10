//! Masks blend between keyframes like the sliders: each mask's local adjustments and a gradient's
//! geometry are numbers (`mask.<i>.<field>`) that are interpolated over the sequence, so a graduated
//! filter darkening the sky can fade out as the sun sets instead of jumping at the next keyframe.
//!
//! Masks are matched by their place in the list. A keyframe without mask `i` counts as having it with
//! no effect (adjustments at zero, geometry of the first keyframe that has it), so a filter added on
//! one keyframe fades in from its neighbours.

use std::collections::BTreeMap;

use lightcraft_engine::develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskShape};

pub use lightcraft_engine::develop::MaskShape as Shape;

/// Prefix of a mask value's id.
pub const PREFIX: &str = "mask.";

type Field = (
    &'static str,
    fn(&LocalAdjustments) -> f64,
    fn(&mut LocalAdjustments, f64),
);

/// The local adjustments that blend (hues, which wrap around, do not).
const ADJUST: &[Field] = &[
    ("exposure", |a| a.exposure, |a, v| a.exposure = v),
    ("temp", |a| a.temp, |a, v| a.temp = v),
    ("tint", |a| a.tint, |a, v| a.tint = v),
    ("contrast", |a| a.contrast, |a, v| a.contrast = v),
    ("highlights", |a| a.highlights, |a, v| a.highlights = v),
    ("shadows", |a| a.shadows, |a, v| a.shadows = v),
    ("whites", |a| a.whites, |a, v| a.whites = v),
    ("blacks", |a| a.blacks, |a, v| a.blacks = v),
    ("texture", |a| a.texture, |a, v| a.texture = v),
    ("clarity", |a| a.clarity, |a, v| a.clarity = v),
    ("dehaze", |a| a.dehaze, |a, v| a.dehaze = v),
    ("saturation", |a| a.saturation, |a, v| a.saturation = v),
];

/// Ranges the blended adjustments are held to (as LightCraft's local sliders).
fn range(field: &str) -> (f64, f64) {
    match field {
        "exposure" => (-4.0, 4.0),
        "startX" | "startY" | "endX" | "endY" | "centerX" | "centerY" => (-2.0, 3.0),
        "rx" | "ry" => (0.0, 4.0),
        "angle" => (-360.0, 360.0),
        "feather" => (0.0, 100.0),
        "lumLo" | "lumHi" | "lumLoFeather" | "lumHiFeather" => (0.0, 1.0),
        _ => (-100.0, 100.0),
    }
}

/// The geometry of a mask that blends: its first component, when it is a linear or radial gradient.
fn geometry(m: &Mask) -> Vec<(&'static str, f64)> {
    match m.components.first().map(|c| &c.shape) {
        Some(MaskShape::Linear { start, end }) => vec![
            ("startX", start.x),
            ("startY", start.y),
            ("endX", end.x),
            ("endY", end.y),
        ],
        Some(MaskShape::Radial {
            center,
            rx,
            ry,
            angle,
            feather,
            ..
        }) => vec![
            ("centerX", center.x),
            ("centerY", center.y),
            ("rx", *rx),
            ("ry", *ry),
            ("angle", *angle),
            ("feather", *feather),
        ],
        Some(MaskShape::LuminanceRange {
            lo,
            hi,
            lo_feather,
            hi_feather,
        }) => vec![
            ("lumLo", *lo),
            ("lumHi", *hi),
            ("lumLoFeather", *lo_feather),
            ("lumHiFeather", *hi_feather),
        ],
        _ => Vec::new(),
    }
}

fn set_geometry(m: &mut Mask, field: &str, v: f64) -> bool {
    let Some(c) = m.components.first_mut() else {
        return false;
    };
    match (&mut c.shape, field) {
        (MaskShape::Linear { start, .. }, "startX") => start.x = v,
        (MaskShape::Linear { start, .. }, "startY") => start.y = v,
        (MaskShape::Linear { end, .. }, "endX") => end.x = v,
        (MaskShape::Linear { end, .. }, "endY") => end.y = v,
        (MaskShape::Radial { center, .. }, "centerX") => center.x = v,
        (MaskShape::Radial { center, .. }, "centerY") => center.y = v,
        (MaskShape::Radial { rx, .. }, "rx") => *rx = v,
        (MaskShape::Radial { ry, .. }, "ry") => *ry = v,
        (MaskShape::Radial { angle, .. }, "angle") => *angle = v,
        (MaskShape::Radial { feather, .. }, "feather") => *feather = v,
        (MaskShape::LuminanceRange { lo, .. }, "lumLo") => *lo = v,
        (MaskShape::LuminanceRange { hi, .. }, "lumHi") => *hi = v,
        (MaskShape::LuminanceRange { lo_feather, .. }, "lumLoFeather") => *lo_feather = v,
        (MaskShape::LuminanceRange { hi_feather, .. }, "lumHiFeather") => *hi_feather = v,
        _ => return false,
    }
    true
}

/// Every blending value of `d`'s masks, by id.
#[must_use]
pub fn values(d: &DevelopSettings) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for (i, m) in d.masks.iter().enumerate() {
        for (name, get, _) in ADJUST {
            out.push((format!("{PREFIX}{i}.{name}"), get(&m.adjust)));
        }
        for (name, v) in geometry(m) {
            out.push((format!("{PREFIX}{i}.{name}"), v));
        }
    }
    out
}

/// The values of `template` (mask `i` of another keyframe) with no effect: adjustments at zero,
/// the same geometry.
#[must_use]
pub fn absent(i: usize, template: &Mask) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = ADJUST
        .iter()
        .map(|(name, ..)| (format!("{PREFIX}{i}.{name}"), 0.0))
        .collect();
    for (name, v) in geometry(template) {
        out.push((format!("{PREFIX}{i}.{name}"), v));
    }
    out
}

/// Sets mask value `id` (`mask.<i>.<field>`) in `d`; whether there was such a value.
pub fn set(d: &mut DevelopSettings, id: &str, v: f64) -> bool {
    let Some(rest) = id.strip_prefix(PREFIX) else {
        return false;
    };
    let Some((i, field)) = rest.split_once('.') else {
        return false;
    };
    let (Ok(i), true) = (i.parse::<usize>(), v.is_finite()) else {
        return false;
    };
    let Some(m) = d.masks.get_mut(i) else { return false };
    let (lo, hi) = range(field);
    let v = v.clamp(lo, hi);
    if let Some((.., put)) = ADJUST.iter().find(|(name, ..)| *name == field) {
        put(&mut m.adjust, v);
        return true;
    }
    set_geometry(m, field, v)
}

/// Gives every settings of `keyframes` the same masks in the same order, matched by name (the
/// n-th mask of a name with the n-th of that name elsewhere): in the order they first appear, a
/// mask a keyframe lacks added there with no effect (the same shape, adjustments at zero), so masks
/// blend by name, however they were added or copied.
pub fn align(keyframes: &mut [DevelopSettings]) {
    // The keys (name, how many of that name came before in its keyframe), in first-seen order.
    let key_of = |masks: &[Mask], i: usize| -> (String, usize) {
        let name = masks.get(i).map(|m| m.name.clone()).unwrap_or_default();
        let before = masks.iter().take(i).filter(|m| m.name == name).count();
        (name, before)
    };
    let mut order: Vec<((String, usize), Mask)> = Vec::new();
    for d in keyframes.iter() {
        for i in 0..d.masks.len() {
            let key = key_of(&d.masks, i);
            if !order.iter().any(|(k, _)| *k == key)
                && let Some(m) = d.masks.get(i)
            {
                order.push((key, m.clone()));
            }
        }
    }
    if order.is_empty() {
        return;
    }
    for d in keyframes.iter_mut() {
        let own: Vec<((String, usize), Mask)> = (0..d.masks.len())
            .filter_map(|i| d.masks.get(i).map(|m| (key_of(&d.masks, i), m.clone())))
            .collect();
        d.masks = order
            .iter()
            .map(|(key, template)| match own.iter().find(|(k, _)| k == key) {
                Some((_, m)) => m.clone(),
                None => {
                    let mut m = template.clone();
                    m.adjust = LocalAdjustments {
                        amount: m.adjust.amount,
                        ..LocalAdjustments::default()
                    };
                    m
                }
            })
            .collect();
    }
}

/// The masks of all `bases` (keyframes' settings), one per place: the first keyframe's that has it.
#[must_use]
pub fn templates(bases: &[&DevelopSettings]) -> Vec<Mask> {
    let mut out: Vec<Mask> = Vec::new();
    for d in bases {
        for (i, m) in d.masks.iter().enumerate() {
            if i >= out.len() {
                out.push(m.clone());
            }
        }
    }
    out
}

/// `d` with every mask of `templates` it lacks added, with no effect (so values can be set on it).
pub fn complete(d: &mut DevelopSettings, templates: &[Mask]) {
    for t in templates.iter().skip(d.masks.len()) {
        let mut m = t.clone();
        m.adjust = LocalAdjustments {
            amount: m.adjust.amount,
            ..LocalAdjustments::default()
        };
        d.masks.push(m);
    }
}

/// Fills in the values of `keyframe` (with masks `own`) for the masks of `templates` it lacks.
pub fn fill_absent(values: &mut BTreeMap<String, f64>, own: usize, templates: &[Mask]) {
    for (i, t) in templates.iter().enumerate().skip(own) {
        for (id, v) in absent(i, t) {
            values.entry(id).or_insert(v);
        }
    }
}

/// The local adjustments the app offers for a mask, with their ranges: id field, low, high.
pub const ADJUSTMENTS: &[(&str, f64, f64)] = &[
    ("exposure", -4.0, 4.0),
    ("temp", -100.0, 100.0),
    ("tint", -100.0, 100.0),
    ("contrast", -100.0, 100.0),
    ("highlights", -100.0, 100.0),
    ("shadows", -100.0, 100.0),
    ("whites", -100.0, 100.0),
    ("blacks", -100.0, 100.0),
    ("texture", -100.0, 100.0),
    ("clarity", -100.0, 100.0),
    ("dehaze", -100.0, 100.0),
    ("saturation", -100.0, 100.0),
];

/// The value `field` of mask `m` (an adjustment or its shape's geometry).
#[must_use]
pub fn get(m: &Mask, field: &str) -> Option<f64> {
    ADJUST
        .iter()
        .find(|(name, ..)| *name == field)
        .map(|(_, get, _)| get(&m.adjust))
        .or_else(|| {
            geometry(m)
                .into_iter()
                .find(|(name, _)| *name == field)
                .map(|(_, v)| v)
        })
}

/// What kind of mask the app makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A linear gradient: full effect at the start, none at the end.
    Linear,
    /// An ellipse, full effect inside.
    Radial,
    /// The sky, found in every frame anew.
    Sky,
    /// A range of brightness (the bright parts: a sky without the foreground).
    Luminance,
    /// Painted by hand.
    Brush,
}

impl Kind {
    /// The kind of `m` (its first component), if the app knows it.
    #[must_use]
    pub fn of(m: &Mask) -> Option<Self> {
        match m.components.first().map(|c| &c.shape) {
            Some(MaskShape::Linear { .. }) => Some(Self::Linear),
            Some(MaskShape::Radial { .. }) => Some(Self::Radial),
            Some(MaskShape::Sky) => Some(Self::Sky),
            Some(MaskShape::LuminanceRange { .. }) => Some(Self::Luminance),
            Some(MaskShape::Brush { .. }) => Some(Self::Brush),
            _ => None,
        }
    }
}

/// A new mask of `kind`, named `name`, with no effect yet (but exposure −0.5 so it shows).
#[must_use]
pub fn new_mask(kind: Kind, name: &str) -> Mask {
    let shape = match kind {
        Kind::Linear => MaskShape::Linear {
            start: lightcraft_geom_point(0.5, 0.0),
            end: lightcraft_geom_point(0.5, 0.55),
        },
        Kind::Radial => MaskShape::Radial {
            center: lightcraft_geom_point(0.5, 0.5),
            rx: 0.2,
            ry: 0.15,
            angle: 0.0,
            feather: 50.0,
            invert: false,
        },
        Kind::Sky => MaskShape::Sky,
        Kind::Brush => MaskShape::Brush { strokes: Vec::new() },
        Kind::Luminance => MaskShape::LuminanceRange {
            lo: 0.6,
            hi: 1.0,
            lo_feather: 0.15,
            hi_feather: 0.0,
        },
    };
    Mask {
        name: name.to_owned(),
        components: vec![MaskComponent {
            name: None,
            op: lightcraft_engine::develop::MaskOp::Add,
            invert: false,
            shape,
        }],
        adjust: LocalAdjustments {
            exposure: -0.5,
            ..LocalAdjustments::default()
        },
        ..Mask::default()
    }
}

/// Most points kept of one stroke (a long drag is thinned to this many).
pub const MAX_STROKE_POINTS: usize = 2000;

/// Adds a stroke through `points` (normalized to the frame) to a brush mask `m`: `size` is the
/// radius as a fraction of the long edge, `feather` 0..100, `erase` takes paint away. Whether `m`
/// is a brush mask and the stroke had points.
pub fn add_stroke(m: &mut Mask, points: &[(f64, f64)], size: f64, feather: f64, erase: bool) -> bool {
    let Some(MaskShape::Brush { strokes }) = m.components.first_mut().map(|c| &mut c.shape) else {
        return false;
    };
    let finite: Vec<(f64, f64)> = points
        .iter()
        .copied()
        .filter(|(x, y)| x.is_finite() && y.is_finite())
        .collect();
    if finite.is_empty() {
        return false;
    }
    let step = finite.len().div_ceil(MAX_STROKE_POINTS).max(1);
    let points = finite
        .iter()
        .step_by(step)
        .map(|(x, y)| lightcraft_geom_point(x.clamp(-0.5, 1.5), y.clamp(-0.5, 1.5)))
        .collect();
    strokes.push(lightcraft_engine::develop::BrushStroke {
        points,
        size: if size.is_finite() {
            size.clamp(0.002, 0.5)
        } else {
            0.03
        },
        feather: if feather.is_finite() {
            feather.clamp(0.0, 100.0)
        } else {
            50.0
        },
        erase,
        ..lightcraft_engine::develop::BrushStroke::default()
    });
    true
}

/// Takes every stroke off a brush mask `m`; whether it had any.
pub fn clear_strokes(m: &mut Mask) -> bool {
    match m.components.first_mut().map(|c| &mut c.shape) {
        Some(MaskShape::Brush { strokes }) if !strokes.is_empty() => {
            strokes.clear();
            true
        }
        _ => false,
    }
}

/// A stroke as drawn: points (0..1 of the frame), radius (fraction of the long edge), erasing.
pub type Stroke = (Vec<(f64, f64)>, f64, bool);

/// The strokes of a brush mask `m`.
#[must_use]
pub fn strokes(m: &Mask) -> Vec<Stroke> {
    match m.components.first().map(|c| &c.shape) {
        Some(MaskShape::Brush { strokes }) => strokes
            .iter()
            .map(|s| (s.points.iter().map(|p| (p.x, p.y)).collect(), s.size, s.erase))
            .collect(),
        _ => Vec::new(),
    }
}

/// Scales every blending adjustment of `m` by `k`.
fn scale_adjustments(m: &mut Mask, k: f64) {
    for (_, get, put) in ADJUST {
        let v = get(&m.adjust);
        put(&mut m.adjust, v * k);
    }
}

/// Brush masks of `d` painted differently in `next` (the keyframe after): the strokes of `d` act
/// with `1 − w` of the adjustments, `next`'s strokes (as another mask) with `w`, so the painting
/// changes smoothly from one keyframe to the next.
pub fn crossfade_brushes(d: &mut DevelopSettings, next: &DevelopSettings, w: f64) {
    let w = if w.is_finite() { w.clamp(0.0, 1.0) } else { 0.0 };
    // On the keyframe itself: its own painting, nothing added (it would be saved with it).
    if w <= 0.0 {
        return;
    }
    let mut added = Vec::new();
    for (m, n) in d.masks.iter_mut().zip(&next.masks) {
        let (Some(MaskShape::Brush { strokes: a }), Some(MaskShape::Brush { strokes: b })) = (
            m.components.first().map(|c| &c.shape),
            n.components.first().map(|c| &c.shape),
        ) else {
            continue;
        };
        if a == b {
            continue;
        }
        let mut incoming = m.clone();
        if let Some(c) = incoming.components.first_mut() {
            c.shape = MaskShape::Brush { strokes: b.clone() };
        }
        scale_adjustments(&mut incoming, w);
        scale_adjustments(m, 1.0 - w);
        added.push(incoming);
    }
    d.masks.extend(added);
}

/// A graduated filter over the top of the frame (the sky): full effect at the top, none at
/// `bottom` (0..1 down the frame), with `exposure` stops.
#[must_use]
pub fn sky_gradient(top: f64, bottom: f64, exposure: f64) -> Mask {
    use lightcraft_engine::develop::MaskOp;
    Mask {
        name: "Lapsewerk sky".into(),
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear {
                start: lightcraft_geom_point(0.5, top),
                end: lightcraft_geom_point(0.5, bottom),
            },
        }],
        adjust: LocalAdjustments {
            exposure,
            ..LocalAdjustments::default()
        },
        ..Mask::default()
    }
}

fn lightcraft_geom_point(x: f64, y: f64) -> lightcraft_geom::Point {
    lightcraft_geom::Point { x, y }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_and_hostile_ids_are_refused() {
        let mut d = DevelopSettings::default();
        d.masks.push(sky_gradient(0.0, 0.5, -1.0));
        let v = values(&d);
        assert!(v.contains(&("mask.0.exposure".to_owned(), -1.0)));
        assert!(v.contains(&("mask.0.endY".to_owned(), 0.5)));
        let mut e = DevelopSettings::default();
        e.masks.push(sky_gradient(0.0, 0.9, 0.0));
        for (id, x) in &v {
            assert!(set(&mut e, id, *x), "{id}");
        }
        assert_eq!(e.masks, d.masks);
        for bad in [
            "mask.1.exposure",
            "mask.x.exposure",
            "mask.0.nothing",
            "mask.0",
            "exposure",
        ] {
            assert!(!set(&mut e, bad, 1.0), "{bad}");
        }
        assert!(!set(&mut e, "mask.0.exposure", f64::NAN));
        assert!(set(&mut e, "mask.0.exposure", 99.0));
        assert_eq!(e.masks[0].adjust.exposure, 4.0);
    }

    #[test]
    fn new_masks_of_every_kind_round_trip() {
        for kind in [Kind::Linear, Kind::Radial, Kind::Sky, Kind::Luminance] {
            let m = new_mask(kind, "m");
            assert_eq!(Kind::of(&m), Some(kind));
            assert_eq!(get(&m, "exposure"), Some(-0.5));
            let mut d = DevelopSettings::default();
            d.masks.push(m);
            for (id, v) in values(&d) {
                assert!(set(&mut d, &id, v), "{id}");
            }
        }
        let mut d = DevelopSettings::default();
        d.masks.push(new_mask(Kind::Luminance, "l"));
        assert!(set(&mut d, "mask.0.lumLo", 0.3));
        assert_eq!(d.masks.first().and_then(|m| get(m, "lumLo")), Some(0.3));
        assert!(set(&mut d, "mask.0.lumHi", 7.0));
        assert_eq!(d.masks.first().and_then(|m| get(m, "lumHi")), Some(1.0));
    }

    #[test]
    fn strokes_are_added_reined_in_and_cleared() {
        let mut m = new_mask(Kind::Brush, "b");
        assert_eq!(Kind::of(&m), Some(Kind::Brush));
        assert!(!add_stroke(&mut m, &[], 0.05, 50.0, false));
        assert!(!add_stroke(&mut m, &[(f64::NAN, 0.5)], 0.05, 50.0, false));
        let long: Vec<(f64, f64)> = (0..10_000).map(|i| (f64::from(i) / 10_000.0, 0.5)).collect();
        assert!(add_stroke(&mut m, &long, 9.0, f64::INFINITY, true));
        let s = strokes(&m);
        assert_eq!(s.len(), 1);
        assert!(s[0].0.len() <= MAX_STROKE_POINTS);
        assert_eq!((s[0].1, s[0].2), (0.5, true));
        assert!(clear_strokes(&mut m));
        assert!(strokes(&m).is_empty());
        assert!(!clear_strokes(&mut m));
        let mut linear = new_mask(Kind::Linear, "l");
        assert!(!add_stroke(&mut linear, &[(0.5, 0.5)], 0.05, 50.0, false));
    }

    #[test]
    fn differently_painted_keyframes_crossfade() {
        let paint = |x: f64| {
            let mut m = new_mask(Kind::Brush, "b");
            m.adjust.exposure = -2.0;
            assert!(add_stroke(&mut m, &[(x, 0.5)], 0.1, 50.0, false));
            m
        };
        let mut d = DevelopSettings::default();
        d.masks.push(paint(0.2));
        let mut next = DevelopSettings::default();
        next.masks.push(paint(0.8));
        let mut mid = d.clone();
        crossfade_brushes(&mut mid, &next, 0.25);
        assert_eq!(mid.masks.len(), 2);
        assert!((mid.masks[0].adjust.exposure + 1.5).abs() < 1e-9);
        assert!((mid.masks[1].adjust.exposure + 0.5).abs() < 1e-9);
        assert_eq!(strokes(&mid.masks[1])[0].0, vec![(0.8, 0.5)]);
        // On the keyframe itself, or the same strokes: nothing to fade.
        let mut on_key = d.clone();
        crossfade_brushes(&mut on_key, &next, 0.0);
        assert_eq!(on_key, d);
        let mut same = d.clone();
        crossfade_brushes(&mut same, &d, 0.5);
        assert_eq!(same, d);
    }

    #[test]
    fn a_keyframe_without_the_mask_has_it_with_no_effect() {
        let mut with = DevelopSettings::default();
        with.masks.push(sky_gradient(0.0, 0.6, -1.5));
        let without = DevelopSettings::default();
        let t = templates(&[&without, &with]);
        assert_eq!(t.len(), 1);
        let mut values = BTreeMap::new();
        fill_absent(&mut values, without.masks.len(), &t);
        assert_eq!(values.get("mask.0.exposure"), Some(&0.0));
        assert_eq!(values.get("mask.0.endY"), Some(&0.6));
        let mut d = without.clone();
        complete(&mut d, &t);
        assert_eq!(d.masks.len(), 1);
        assert_eq!(d.masks[0].adjust.exposure, 0.0);
        assert!(set(&mut d, "mask.0.exposure", -0.75));
    }
}

//! Every develop slider of a keyframe, in the groups common raw editors use: built from LightCraft's control
//! specs (range, default, decimals), named in German or English. All of them blend between
//! keyframes (`lapsewerk_develop::Sequence::keyframe` takes every numeric control).

use lapsewerk_develop::controls::{self, ControlSpec};

use crate::text::Lang;

/// A group of sliders.
pub struct Group {
    pub de: &'static str,
    pub en: &'static str,
    /// Open when the editor opens.
    pub open: bool,
    /// The controls: these ids in this order, then every control starting with one of `prefixes`.
    pub ids: &'static [&'static str],
    pub prefixes: &'static [&'static str],
}

impl Group {
    #[must_use]
    pub fn name(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::De => self.de,
            Lang::En => self.en,
        }
    }

    /// The group's controls, in panel order.
    #[must_use]
    pub fn specs(&self) -> Vec<&'static ControlSpec> {
        let mut out: Vec<&'static ControlSpec> =
            self.ids.iter().filter_map(|id| controls::find(id)).collect();
        out.extend(
            controls::CONTROLS
                .iter()
                .filter(|c| self.prefixes.iter().any(|p| c.id.starts_with(p))),
        );
        out
    }
}

/// The groups. Left out: HDR output (the export sets it), black-and-white mixing, profile, geometry
/// and crop (framing is the video's, step 4).
pub const GROUPS: &[Group] = &[
    Group {
        de: "Grundeinstellungen",
        en: "Basic",
        open: true,
        ids: &[
            "light.exposure",
            "wb.temp",
            "wb.tint",
            "light.contrast",
            "light.highlights",
            "light.shadows",
            "light.whites",
            "light.blacks",
            "color.vibrance",
            "color.saturation",
        ],
        prefixes: &[],
    },
    Group {
        de: "Präsenz",
        en: "Presence",
        open: false,
        ids: &["effects.texture", "effects.clarity", "effects.dehaze"],
        prefixes: &[],
    },
    Group {
        de: "Gradationskurve",
        en: "Tone curve",
        open: false,
        ids: &[],
        prefixes: &["curve."],
    },
    Group {
        de: "Farbmischer (HSL)",
        en: "Color mixer (HSL)",
        open: false,
        ids: &[],
        prefixes: &["mixer."],
    },
    Group {
        de: "Farbtönung (Color Grading)",
        en: "Color grading",
        open: false,
        ids: &[],
        prefixes: &["grading."],
    },
    Group {
        de: "Details",
        en: "Detail",
        open: false,
        ids: &["enhance.denoise"],
        prefixes: &["detail."],
    },
    Group {
        de: "Objektiv",
        en: "Optics",
        open: false,
        ids: &[],
        prefixes: &["optics."],
    },
    Group {
        de: "Vignette und Körnung",
        en: "Vignette and grain",
        open: false,
        ids: &[],
        prefixes: &["vignette.", "grain."],
    },
    Group {
        de: "Kalibrierung",
        en: "Calibration",
        open: false,
        ids: &[],
        prefixes: &["calibration."],
    },
];

/// What can be copied to other keyframes: a name, its controls (or the masks), and whether it is
/// ticked at first. Exposure and white balance are not: they differ from keyframe to keyframe.
pub struct Part {
    pub de: &'static str,
    pub en: &'static str,
    pub ids: Vec<&'static str>,
    pub masks: bool,
    pub on: bool,
}

impl Part {
    #[must_use]
    pub fn name(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::De => self.de,
            Lang::En => self.en,
        }
    }
}

/// The parts, in panel order: the basic group split up, then the other groups, then the masks.
#[must_use]
pub fn copy_parts() -> Vec<Part> {
    let part = |de, en, ids: &[&'static str], on| Part {
        de,
        en,
        ids: ids.to_vec(),
        masks: false,
        on,
    };
    let mut out = vec![
        part("Belichtung", "Exposure", &["light.exposure"], false),
        part("Weißabgleich", "White balance", &["wb.temp", "wb.tint"], false),
        part(
            "Kontrast, Lichter, Tiefen, Weiß, Schwarz",
            "Contrast, highlights, shadows, whites, blacks",
            &[
                "light.contrast",
                "light.highlights",
                "light.shadows",
                "light.whites",
                "light.blacks",
            ],
            true,
        ),
        part(
            "Dynamik und Sättigung",
            "Vibrance and saturation",
            &["color.vibrance", "color.saturation"],
            true,
        ),
    ];
    for g in GROUPS.iter().skip(1) {
        out.push(Part {
            de: g.de,
            en: g.en,
            ids: g.specs().into_iter().map(|s| s.id).collect(),
            masks: false,
            on: true,
        });
    }
    out.push(Part {
        de: "Masken (Verläufe, Filter)",
        en: "Masks (gradients, filters)",
        ids: Vec::new(),
        masks: true,
        on: true,
    });
    out
}

/// The ids the groups show.
#[cfg(test)]
fn all_ids() -> Vec<&'static str> {
    GROUPS
        .iter()
        .flat_map(|g| g.specs().into_iter().map(|s| s.id))
        .collect()
}

const COLORS: [(&str, &str); 8] = [
    ("red", "Rot"),
    ("orange", "Orange"),
    ("yellow", "Gelb"),
    ("green", "Grün"),
    ("aqua", "Aquamarin"),
    ("blue", "Blau"),
    ("purple", "Lila"),
    ("magenta", "Magenta"),
];

/// A control's name.
#[must_use]
pub fn label(spec: &ControlSpec, lang: Lang) -> String {
    if lang == Lang::En {
        return spec.label.to_owned();
    }
    let id = spec.id;
    let part = |name: &str| match name {
        "hue" => "Farbton",
        "sat" => "Sättigung",
        _ => "Luminanz",
    };
    let parts: Vec<&str> = id.split('.').collect();
    match parts.as_slice() {
        ["mixer", color, what] => {
            let c = COLORS
                .iter()
                .find(|(e, _)| e == color)
                .map_or(*color, |(_, d)| *d);
            return format!("{c} {}", part(what));
        }
        [
            "grading",
            zone @ ("shadows" | "midtones" | "highlights" | "global"),
            what,
        ] => {
            let z = match *zone {
                "shadows" => "Tiefen",
                "midtones" => "Mitteltöne",
                "highlights" => "Lichter",
                _ => "Global",
            };
            return format!("{z} {}", part(what));
        }
        _ => {}
    }
    let de = match id {
        "light.exposure" => "Belichtung",
        "wb.temp" => "Farbtemperatur",
        "wb.tint" => "Tönung",
        "light.contrast" => "Kontrast",
        "light.highlights" | "curve.highlights" => "Lichter",
        "light.shadows" | "curve.shadows" => "Tiefen",
        "light.whites" => "Weiß",
        "light.blacks" => "Schwarz",
        "color.vibrance" => "Dynamik",
        "color.saturation" => "Sättigung",
        "effects.texture" => "Struktur",
        "effects.clarity" => "Klarheit",
        "effects.dehaze" => "Dunst entfernen",
        "curve.lights" => "Helle Mitteltöne",
        "curve.darks" => "Dunkle Mitteltöne",
        "curve.splitShadows" => "Grenze Tiefen",
        "curve.splitMid" => "Grenze Mitte",
        "curve.splitHighlights" => "Grenze Lichter",
        "curve.refineSaturation" => "Sättigung anpassen",
        "grading.blending" => "Überblendung",
        "grading.balance" => "Abgleich",
        "enhance.denoise" => "KI-Rauschentfernung",
        "detail.sharpenAmount" => "Schärfen",
        "detail.sharpenRadius" => "Radius",
        "detail.sharpenDetail" | "detail.nrDetail" | "detail.nrColorDetail" => "Details",
        "detail.sharpenMasking" => "Maskieren",
        "detail.nrLuminance" => "Rauschreduzierung",
        "detail.nrContrast" => "Kontrast",
        "detail.nrColor" => "Farbrauschen",
        "detail.nrColorSmoothness" => "Glättung",
        "optics.distortion" => "Verzeichnung",
        "optics.vignetting" => "Vignettierung",
        "optics.vignettingMidpoint" => "Mittelpunkt",
        "optics.profileDistortion" => "Profil: Verzeichnung",
        "optics.profileVignetting" => "Profil: Vignettierung",
        "optics.defringePurple" => "Lila Säume",
        "optics.defringePurpleHueLo" => "Lila Farbton von",
        "optics.defringePurpleHueHi" => "Lila Farbton bis",
        "optics.defringeGreen" => "Grüne Säume",
        "optics.defringeGreenHueLo" => "Grün Farbton von",
        "optics.defringeGreenHueHi" => "Grün Farbton bis",
        "optics.caRed" => "Rot/Cyan-Saum",
        "optics.caBlue" => "Blau/Gelb-Saum",
        "vignette.amount" => "Vignette",
        "vignette.midpoint" => "Mittelpunkt",
        "vignette.roundness" => "Rundheit",
        "vignette.feather" => "Weiche Kante",
        "vignette.highlights" => "Lichter",
        "grain.amount" => "Körnung",
        "grain.size" => "Größe",
        "grain.roughness" => "Unregelmäßigkeit",
        "calibration.shadowsTint" => "Tiefen Tönung",
        "calibration.redHue" => "Rot Farbton",
        "calibration.redSat" => "Rot Sättigung",
        "calibration.greenHue" => "Grün Farbton",
        "calibration.greenSat" => "Grün Sättigung",
        "calibration.blueHue" => "Blau Farbton",
        "calibration.blueSat" => "Blau Sättigung",
        _ => spec.label,
    };
    de.to_owned()
}

/// A few plain words on a group of settings whose name is a term of the trade, for the copy
/// dialog (where it is ticked without its sliders in view).
#[must_use]
pub fn part_hint(de: &str, lang: crate::text::Lang) -> Option<&'static str> {
    let (de_hint, en_hint) = match de {
        "Präsenz" => ("Struktur, Klarheit, Dunst", "texture, clarity, haze"),
        "Gradationskurve" => ("Helligkeit einzelner Tonbereiche", "brightness of tone ranges"),
        "Farbmischer (HSL)" => ("einzelne Farben ändern", "change single colours"),
        "Farbtönung (Color Grading)" => (
            "Farbstich für Tiefen, Mitten, Lichter",
            "tint for shadows, midtones, highlights",
        ),
        "Details" => ("Schärfe, Rauschen", "sharpening, noise"),
        "Objektiv" => (
            "Verzeichnung, Randabdunklung",
            "distortion, vignetting of the lens",
        ),
        "Vignette und Körnung" => ("dunkle Ecken, Filmkorn", "dark corners, film grain"),
        "Kalibrierung" => ("Grundfarben der Kamera", "the camera's primary colours"),
        _ => return None,
    };
    Some(if lang == crate::text::Lang::De {
        de_hint
    } else {
        en_hint
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copy_parts_cover_every_slider_once() {
        let parts = copy_parts();
        let mut ids: Vec<&str> = parts.iter().flat_map(|p| p.ids.iter().copied()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "a slider in two parts");
        let mut all = all_ids();
        all.sort_unstable();
        assert_eq!(ids, all);
        assert!(
            !parts[0].on && !parts[1].on,
            "exposure and white balance stay off"
        );
        assert!(parts.last().is_some_and(|p| p.masks));
    }

    #[test]
    fn every_group_has_controls_and_every_control_a_german_name() {
        for g in GROUPS {
            assert!(!g.specs().is_empty(), "{}", g.en);
        }
        let ids = all_ids();
        assert!(ids.len() > 80, "{}", ids.len());
        let mut seen = std::collections::HashSet::new();
        for id in &ids {
            assert!(seen.insert(*id), "{id} twice");
            let spec = controls::find(id).unwrap();
            // The same word in both languages.
            if !["vignette.amount", "detail.sharpenRadius"].contains(id) {
                assert_ne!(label(spec, Lang::De), spec.label, "{id} has no German name");
            }
        }
    }
}

//! HDR video: the PQ (SMPTE ST 2084) and HLG (ITU-R BT.2100) signals, from frames rendered with
//! highlights above SDR white.
//!
//! An HDR frame is carried through Lapsewerk's 16-bit stages like an SDR one: its linear light,
//! divided by the render's peak, encoded with the sRGB curve ([`encode_frame`]). Every stage works in
//! linear light (it decodes the sRGB curve first), and a common scale does not change what they do,
//! so dust, regional deflicker, noise reduction and blending need no HDR version. Only the encoder's
//! table ([`signal_table`]) turns the values into the HDR signal.
//!
//! Levels follow ITU-R BT.2408: SDR white (1.0 in the render) is 203 cd/m²; HLG's nominal peak is
//! 1000 cd/m² with a system gamma of 1.2, so SDR white lands at 75 % of the HLG signal.

/// Luminance of SDR white (graphics white) in an HDR signal, cd/m² (ITU-R BT.2408).
pub const SDR_WHITE_NITS: f64 = 203.0;

/// Nominal peak of an HLG display, cd/m² (ITU-R BT.2100 reference).
pub const HLG_PEAK_NITS: f64 = 1000.0;

/// HLG system gamma at [`HLG_PEAK_NITS`].
pub const HLG_GAMMA: f64 = 1.2;

/// The HDR signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Perceptual quantizer: absolute luminance up to 10 000 cd/m² (HDR10, Dolby Vision base).
    Pq,
    /// Hybrid log-gamma: relative to the display's peak; looks right on SDR screens too.
    Hlg,
}

impl Signal {
    /// Highest headroom above SDR white, in stops, the signal can carry: PQ to 10 000 cd/m² (5.6
    /// stops above 203), HLG to its nominal 1000 cd/m² (2.3 stops).
    #[must_use]
    pub fn max_headroom_ev(self) -> f64 {
        match self {
            Self::Pq => (10_000.0 / SDR_WHITE_NITS).log2(),
            Self::Hlg => (HLG_PEAK_NITS / SDR_WHITE_NITS).log2(),
        }
    }
}

/// The PQ signal (0..1) of a luminance in cd/m² (ST 2084 inverse EOTF).
#[must_use]
pub fn pq_encode(nits: f64) -> f64 {
    const M1: f64 = 2610.0 / 16384.0;
    const M2: f64 = 2523.0 / 4096.0 * 128.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 4096.0 * 32.0;
    const C3: f64 = 2392.0 / 4096.0 * 32.0;
    let y = (nits / 10_000.0).clamp(0.0, 1.0);
    let p = y.powf(M1);
    ((C1 + C2 * p) / (1.0 + C3 * p)).powf(M2)
}

/// The HLG signal (0..1) of a scene light (0..1) (BT.2100 OETF).
#[must_use]
pub fn hlg_oetf(e: f64) -> f64 {
    const A: f64 = 0.178_832_77;
    const B: f64 = 0.284_668_92;
    const C: f64 = 0.559_910_73;
    let e = e.clamp(0.0, 1.0);
    if e <= 1.0 / 12.0 {
        (3.0 * e).sqrt()
    } else {
        A * (12.0 * e - B).ln() + C
    }
}

/// The HLG signal of grey display light `nits` (each channel of a grey pixel): the display's OOTF
/// undone (gamma 1.2 at 1000 cd/m²), then the OETF. Light above the nominal peak is clipped.
/// Colours go through [`hlg_encode_pixel`].
#[must_use]
pub fn hlg_encode(nits: f64) -> f64 {
    let display = (nits / HLG_PEAK_NITS).clamp(0.0, 1.0);
    hlg_oetf(display.powf(1.0 / HLG_GAMMA))
}

/// The HLG signal of a pixel of display light (`nits`, Rec. 2020 R, G, B) by ITU-R BT.2100: the
/// display's OOTF (gamma 1.2 at 1000 cd/m², applied to the pixel's luminance, so a colour keeps
/// its hue and saturation) undone, then the OETF on each channel. Light above the nominal peak is
/// clipped. A grey pixel gets what [`hlg_encode`] gives each channel.
#[must_use]
pub fn hlg_encode_pixel(nits: [f64; 3]) -> [f64; 3] {
    let d = nits.map(|n| {
        if n.is_finite() {
            (n / HLG_PEAK_NITS).clamp(0.0, 1.0)
        } else {
            0.0
        }
    });
    // Rec. 2020 luminance of the display light.
    let y = 0.2627 * d[0] + 0.6780 * d[1] + 0.0593 * d[2];
    if y <= 0.0 {
        return [0.0; 3];
    }
    // Display Fd = Ys^(γ-1) · Es with Yd = Ys^γ: the scene light is Es = Fd · Yd^((1-γ)/γ).
    let k = y.powf((1.0 - HLG_GAMMA) / HLG_GAMMA);
    d.map(|c| hlg_oetf((c * k).clamp(0.0, 1.0)))
}

/// sRGB curve, linear 0..1 → encoded 0..1.
fn srgb_encode(l: f64) -> f64 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB curve, encoded 0..1 → linear 0..1.
fn srgb_decode(e: f64) -> f64 {
    if e <= 0.040_45 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear HDR samples (SDR white = 1, up to `peak`) as the 16-bit values the stages carry: the
/// sRGB curve of `value / peak`.
#[must_use]
pub fn encode_frame(linear: &[f32], peak: f32) -> Vec<u16> {
    let peak = if peak.is_finite() && peak >= 1.0 {
        f64::from(peak)
    } else {
        1.0
    };
    // A table over the linear range is faster than powf per sample: 4096 steps, then interpolated.
    const STEPS: usize = 4096;
    let table: Vec<f64> = (0..=STEPS)
        .map(|i| srgb_encode((i as f64 / STEPS as f64).powi(2)))
        .collect();
    linear
        .iter()
        .map(|&v| {
            let r = if v.is_finite() {
                (f64::from(v) / peak).clamp(0.0, 1.0)
            } else {
                0.0
            };
            // Indexed by √r: even steps in the sRGB curve's terms.
            let x = r.sqrt() * STEPS as f64;
            let i = (x as usize).min(STEPS - 1);
            let f = x - i as f64;
            let (a, b) = (
                table.get(i).copied().unwrap_or(0.0),
                table.get(i + 1).copied().unwrap_or(1.0),
            );
            ((a + (b - a) * f) * 65535.0).round().clamp(0.0, 65535.0) as u16
        })
        .collect()
}

/// Linear light (SDR white 1.0) of each 16-bit value [`encode_frame`] made with this `peak` (SDR
/// frames: `peak` 1).
#[must_use]
pub fn linear_table(peak: f32) -> Vec<f32> {
    let peak = if peak.is_finite() && peak >= 1.0 {
        f64::from(peak)
    } else {
        1.0
    };
    (0..=u16::MAX)
        .map(|v| (srgb_decode(f64::from(v) / 65535.0) * peak) as f32)
        .collect()
}

/// Table from the 16-bit values [`encode_frame`] made (with this `peak`) to the 16-bit HDR signal.
#[must_use]
pub fn signal_table(signal: Signal, peak: f32) -> Vec<u16> {
    let peak = if peak.is_finite() && peak >= 1.0 {
        f64::from(peak)
    } else {
        1.0
    };
    (0..=u16::MAX)
        .map(|v| {
            let nits = srgb_decode(f64::from(v) / 65535.0) * peak * SDR_WHITE_NITS;
            let s = match signal {
                Signal::Pq => pq_encode(nits),
                Signal::Hlg => hlg_encode(nits),
            };
            (s * 65535.0).round().clamp(0.0, 65535.0) as u16
        })
        .collect()
}

/// HDR10 static metadata of a PQ movie whose highlights reach `peak` × SDR white.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hdr10Metadata {
    /// SMPTE ST 2086 mastering display colour volume, as the HEVC SEI message carries it (and Apple's
    /// `kVTCompressionPropertyKey_MasteringDisplayColorVolume` takes it): primaries G, B, R and the
    /// white point as x, y in units of 0.00002 (u16), then the maximum and minimum luminance in units
    /// of 0.0001 cd/m² (u32), all big-endian. A P3-D65 display of 1000 (4000, 10 000) cd/m², 0.0001.
    pub mastering_display: [u8; 24],
    /// CTA-861.3 content light level: MaxCLL (the highest the content can reach, cd/m²: an upper bound)
    /// and MaxFALL (0: not measured), u16 big-endian each.
    pub content_light_level: [u8; 4],
}

impl Hdr10Metadata {
    #[must_use]
    pub fn for_peak(peak: f32) -> Self {
        let peak = if peak.is_finite() && peak >= 1.0 {
            f64::from(peak)
        } else {
            1.0
        };
        let nits = (peak * SDR_WHITE_NITS).clamp(1.0, 10_000.0);
        // The display it is mastered for: a real one, as deliveries name it (P3 primaries, D65,
        // 1000 cd/m², or 4000 / 10 000 when the highlights go further), not the content's peak.
        let display: f64 = if nits <= 1000.0 {
            1000.0
        } else if nits <= 4000.0 {
            4000.0
        } else {
            10_000.0
        };
        let xy = |v: f64| ((v / 0.000_02).round() as u16).to_be_bytes();
        let mut md = [0u8; 24];
        // G, B, R, white point (Display P3 primaries, D65).
        let points = [(0.265, 0.690), (0.150, 0.060), (0.680, 0.320), (0.3127, 0.3290)];
        for (chunk, (x, y)) in md.as_chunks_mut::<4>().0.iter_mut().zip(points) {
            let (a, b) = chunk.split_at_mut(2);
            a.copy_from_slice(&xy(x));
            b.copy_from_slice(&xy(y));
        }
        let (_, lum) = md.split_at_mut(16);
        let (max, min) = lum.split_at_mut(4);
        max.copy_from_slice(&((display * 10_000.0).round() as u32).to_be_bytes());
        min.copy_from_slice(&1u32.to_be_bytes());
        let mut cll = [0u8; 4];
        let (max_cll, _max_fall) = cll.split_at_mut(2);
        max_cll.copy_from_slice(&(nits.ceil() as u16).to_be_bytes());
        Self {
            mastering_display: md,
            content_light_level: cll,
        }
    }
}

/// The light a PQ movie actually reaches (CTA-861.3), measured frame by frame as it is written:
/// MaxCLL, the brightest pixel (its brightest of R, G, B) of the whole movie, and MaxFALL, the
/// highest frame average of that, both in cd/m².
#[derive(Debug, Clone)]
pub struct LightMeter {
    /// cd/m² of each 16-bit value [`encode_frame`] makes (with the render's peak).
    nits: Vec<f32>,
    max_cll: f32,
    max_fall: f64,
    frames: u64,
}

/// What a [`LightMeter`] measured, cd/m² (rounded up, as the metadata carries it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightLevel {
    pub max_cll: u16,
    pub max_fall: u16,
    /// Frames measured.
    pub frames: u64,
}

impl LightLevel {
    /// The CTA-861.3 content light level: MaxCLL, MaxFALL, u16 big-endian each.
    #[must_use]
    pub fn bytes(&self) -> [u8; 4] {
        let [a, b] = self.max_cll.to_be_bytes();
        let [c, d] = self.max_fall.to_be_bytes();
        [a, b, c, d]
    }
}

impl LightMeter {
    /// A meter for frames [`encode_frame`] made with this `peak`.
    #[must_use]
    pub fn new(peak: f32) -> Self {
        let peak = if peak.is_finite() && peak >= 1.0 {
            f64::from(peak)
        } else {
            1.0
        };
        let nits = (0..=u16::MAX)
            .map(|v| (srgb_decode(f64::from(v) / 65535.0) * peak * SDR_WHITE_NITS).min(10_000.0) as f32)
            .collect();
        Self {
            nits,
            max_cll: 0.0,
            max_fall: 0.0,
            frames: 0,
        }
    }

    /// One frame's brightest pixel and average (cd/m²), as [`Self::frame`] measures them, without
    /// adding them. `None` for an empty window.
    #[must_use]
    pub fn measure(&self, rgb: &[u16], stride: usize, left: usize, width: usize) -> Option<(f32, f64)> {
        let mut sum = 0.0f64;
        let mut count = 0u64;
        let mut max = 0.0f32;
        for row in rgb.chunks(stride.max(1)) {
            let Some(row) = row.get(left * 3..(left + width) * 3) else {
                continue;
            };
            let mut row_sum = 0.0f32;
            for &[r, g, b] in row.as_chunks::<3>().0 {
                let n = self
                    .nits
                    .get(usize::from(r.max(g).max(b)))
                    .copied()
                    .unwrap_or(0.0);
                max = max.max(n);
                row_sum += n;
            }
            sum += f64::from(row_sum);
            count += width as u64;
        }
        (count > 0).then(|| (max, sum / count as f64))
    }

    /// Adds a frame measured before (its brightest pixel and average, cd/m²).
    pub fn add(&mut self, max: f32, average: f64) {
        if max.is_finite() && average.is_finite() {
            self.max_cll = self.max_cll.max(max);
            self.max_fall = self.max_fall.max(average);
            self.frames += 1;
        }
    }

    /// Measures one frame: rows of `width` RGB pixels (3 samples each), `width` pixels of every row
    /// from `left` on (`stride` samples a row).
    pub fn frame(&mut self, rgb: &[u16], stride: usize, left: usize, width: usize) {
        if let Some((max, average)) = self.measure(rgb, stride, left, width) {
            self.add(max, average);
        }
    }

    /// What was measured (`None` before the first frame).
    #[must_use]
    pub fn level(&self) -> Option<LightLevel> {
        (self.frames > 0).then(|| LightLevel {
            max_cll: f64::from(self.max_cll).ceil().clamp(0.0, 10_000.0) as u16,
            max_fall: self.max_fall.ceil().clamp(0.0, 10_000.0) as u16,
            frames: self.frames,
        })
    }
}

/// Bytes as lowercase hexadecimal (how the encoder helper takes them).
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr10_metadata_names_a_p3_d65_display_and_the_peak() {
        let m = Hdr10Metadata::for_peak(8.0);
        let u16_at = |i: usize| u16::from_be_bytes([m.mastering_display[i], m.mastering_display[i + 1]]);
        // G (0.265, 0.690), B (0.150, 0.060), R (0.680, 0.320), D65 (0.3127, 0.3290).
        let expect = [13250, 34500, 7500, 3000, 34000, 16000, 15635, 16450];
        for (k, e) in expect.iter().enumerate() {
            assert_eq!(u16_at(k * 2), *e, "value {k}");
        }
        let max = u32::from_be_bytes(m.mastering_display[16..20].try_into().unwrap());
        let min = u32::from_be_bytes(m.mastering_display[20..24].try_into().unwrap());
        // 8 × 203 = 1624 cd/m² of content: a 4000 cd/m² display.
        assert_eq!((max, min), (40_000_000, 1));
        let low = Hdr10Metadata::for_peak(4.0);
        assert_eq!(
            u32::from_be_bytes(low.mastering_display[16..20].try_into().unwrap()),
            10_000_000
        );
        assert_eq!(m.content_light_level, [0x06, 0x58, 0, 0]); // MaxCLL 1624, MaxFALL unknown
        assert_eq!(hex(&m.content_light_level), "06580000");
        assert_eq!(hex(&m.mastering_display).len(), 48);
    }

    #[test]
    fn the_light_meter_finds_the_brightest_pixel_and_the_brightest_frame() {
        // Peak 4 × SDR white: value 65535 is 812 cd/m², SDR white (1/4 linear) 203.
        let mut m = LightMeter::new(4.0);
        assert!(m.level().is_none());
        let quarter = (srgb_encode(0.25) * 65535.0).round() as u16;
        // A 4 × 2 frame of SDR white with one pixel at the peak (only its red channel).
        let mut a = vec![quarter; 4 * 2 * 3];
        a[3] = 65535;
        m.frame(&a, 4 * 3, 0, 4);
        // A dark frame.
        m.frame(&[0; 4 * 2 * 3], 4 * 3, 0, 4);
        let l = m.level().unwrap();
        assert_eq!(l.max_cll, 812);
        // (7 × 203 + 812) / 8 = 279.1
        assert_eq!(l.max_fall, 280);
        assert_eq!(l.frames, 2);
        assert_eq!(l.bytes(), [0x03, 0x2c, 0x01, 0x18]);
        // Only the window counts: the peak pixel at x = 1 is left out.
        let mut w = LightMeter::new(4.0);
        w.frame(&a, 4 * 3, 2, 2);
        assert!((203..=204).contains(&w.level().unwrap().max_cll));
        // Never past PQ's 10 000 cd/m².
        let top = Hdr10Metadata::for_peak(1000.0);
        assert_eq!(
            u16::from_be_bytes([top.content_light_level[0], top.content_light_level[1]]),
            10_000
        );
    }

    #[test]
    fn pq_matches_the_reference_values() {
        // ST 2084 / BT.2100 reference points.
        for (nits, signal) in [
            (0.0, 0.0),
            (100.0, 0.5081),
            (203.0, 0.5807),
            (1000.0, 0.7518),
            (10_000.0, 1.0),
        ] {
            assert!(
                (pq_encode(nits) - signal).abs() < 2e-4,
                "{nits} cd/m²: {}",
                pq_encode(nits)
            );
        }
    }

    #[test]
    fn hlg_puts_sdr_white_at_75_percent() {
        assert!((hlg_oetf(1.0 / 12.0) - 0.5).abs() < 1e-9);
        assert!((hlg_oetf(1.0) - 1.0).abs() < 1e-6);
        assert!(
            (hlg_encode(SDR_WHITE_NITS) - 0.75).abs() < 0.005,
            "{}",
            hlg_encode(SDR_WHITE_NITS)
        );
        assert!((hlg_encode(HLG_PEAK_NITS) - 1.0).abs() < 1e-6);
        assert!(hlg_encode(5000.0) <= 1.0);
        // A grey pixel as each channel; a colour by its luminance (BT.2100), not channel by
        // channel: saturated blue keeps its light.
        let grey = hlg_encode_pixel([203.0; 3]);
        assert!(grey.iter().all(|c| (c - hlg_encode(203.0)).abs() < 1e-12));
        let blue = hlg_encode_pixel([0.0, 0.0, 300.0]);
        let back = {
            // Decode: inverse OETF, then the OOTF on the luminance.
            let inv = |e: f64| {
                if e <= 0.5 {
                    e * e / 3.0
                } else {
                    ((e - 0.559_910_73) / 0.178_832_77).exp() / 12.0 + 0.284_668_92 / 12.0
                }
            };
            let es = blue.map(inv);
            let ys = 0.2627 * es[0] + 0.6780 * es[1] + 0.0593 * es[2];
            es[2] * ys.powf(HLG_GAMMA - 1.0) * HLG_PEAK_NITS
        };
        assert!((back - 300.0).abs() < 1.0, "{back}");
        assert!((Signal::Hlg.max_headroom_ev() - 2.30).abs() < 0.01);
        assert!((Signal::Pq.max_headroom_ev() - 5.62).abs() < 0.01);
    }

    #[test]
    fn a_frame_reaches_the_signal_it_should() {
        // Peak 8 (3 stops): SDR white and the peak through encode_frame and the table.
        let peak = 8.0;
        let frame = encode_frame(&[0.0, 1.0, 8.0, 20.0, f32::NAN, -1.0], peak);
        let pq = signal_table(Signal::Pq, peak);
        let at = |v: u16| f64::from(pq[usize::from(v)]) / 65535.0;
        assert_eq!(frame[0], 0);
        assert!((at(frame[1]) - pq_encode(203.0)).abs() < 1e-3, "{}", at(frame[1]));
        assert!((at(frame[2]) - pq_encode(8.0 * 203.0)).abs() < 1e-3);
        assert_eq!(frame[3], frame[2], "above the peak: clipped");
        assert_eq!((frame[4], frame[5]), (0, 0));
        let hlg = signal_table(Signal::Hlg, peak);
        assert!((f64::from(hlg[usize::from(frame[1])]) / 65535.0 - 0.75).abs() < 0.006);
        for t in [&pq, &hlg] {
            assert!(t.windows(2).all(|w| w[0] <= w[1]), "monotone");
        }
    }

    #[test]
    fn the_frame_table_is_close_to_the_exact_curve() {
        let peak = 4.0f32;
        let values: Vec<f32> = (0..=2000).map(|i| i as f32 / 2000.0 * peak).collect();
        let fast = encode_frame(&values, peak);
        for (v, got) in values.iter().zip(fast) {
            let exact = (srgb_encode(f64::from(*v / peak)) * 65535.0).round();
            assert!((f64::from(got) - exact).abs() <= 8.0, "{v}: {got} vs {exact}");
        }
    }
}

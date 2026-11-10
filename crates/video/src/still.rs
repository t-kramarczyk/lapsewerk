//! Frame files for finishing and grading: OpenEXR (half float, linear light, ZIP compressed) and DPX
//! (10-bit, the movie's transfer). Both written here in plain Rust from their published layouts
//! (OpenEXR file layout; SMPTE ST 268 for DPX), checked against ffmpeg's readers in the tests.

use std::io::Write as _;

/// Primaries and white point as OpenEXR's `chromaticities` carries them: red, green, blue, white
/// (x, y each).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chromaticities(pub [f32; 8]);

impl Chromaticities {
    /// Rec. 709 / sRGB, D65.
    pub const REC709: Self = Self([0.64, 0.33, 0.30, 0.60, 0.15, 0.06, 0.3127, 0.3290]);
    /// Rec. 2020, D65 (the HDR frames).
    pub const REC2020: Self = Self([0.708, 0.292, 0.170, 0.797, 0.131, 0.046, 0.3127, 0.3290]);
}

/// When a frame is shown: the rate, its number in the sequence, and its timecode.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Timing {
    /// Frames a second (numerator, denominator); `None`: not stated.
    pub fps: Option<(u32, u32)>,
    /// The frame's number in the sequence (from 0).
    pub index: u32,
    /// The timecode of the sequence's first frame, as a frame count at the whole-frame rate
    /// (non-drop); `None`: no timecode.
    pub start: Option<u32>,
    /// The timecode counts drop-frame (29.97, 59.94): see [`crate::drop_frame_time`].
    pub drop: bool,
}

impl Timing {
    /// This frame's timecode as SMPTE 12M BCD (`HH MM SS FF`, a byte each, as DPX and OpenEXR
    /// store it), counted from [`Self::start`] at the rate rounded to whole frames.
    #[must_use]
    pub fn timecode_bcd(&self) -> Option<u32> {
        let start = self.start?;
        let (n, d) = self.fps?;
        let base = (f64::from(n) / f64::from(d.max(1))).round() as u32;
        if base == 0 {
            return None;
        }
        // The SMPTE 12M word counts frames 0..29 (two bits for the tens; bit 6 is the drop-frame
        // flag). Over 30 frames a second (50, 59.94, 60) it counts frame pairs and marks the
        // second of a pair with the field bit, as ST 12-1 has it: bit 31 in the 25-frame
        // system (50), bit 15 in the 30-frame one (59.94, 60), as ffmpeg reads DPX. OpenEXR
        // keeps its time code in the 30-frame packing: [`Self::timecode_exr`]. Rates over 60
        // have no timecode there.
        if base > 60 {
            return None;
        }
        let count = start.checked_add(self.index)?;
        let (hour, min, sec, f) = if self.drop && (base == 30 || base == 60) {
            crate::drop_frame_time(count, base)
        } else {
            let c = count % (24 * 3600 * base);
            let (f, s) = (c % base, c / base);
            (s / 3600, (s / 60) % 60, s % 60, f)
        };
        let (f, phase) = if base > 30 { (f / 2, f % 2) } else { (f, 0) };
        // Bit 6: drop-frame.
        let df = u32::from(self.drop && (base == 30 || base == 60)) << 6;
        let bcd = |v: u32| ((v / 10) << 4) | (v % 10);
        let field = if base.is_multiple_of(25) {
            phase << 31
        } else {
            phase << 15
        };
        Some((bcd(hour) << 24) | (bcd(min) << 16) | (bcd(sec) << 8) | bcd(f) | df | field)
    }

    /// [`Self::timecode_bcd`] as OpenEXR stores it: its `timeCode` attribute uses the 30-frame
    /// packing at every rate, the field phase in bit 15.
    #[must_use]
    pub fn timecode_exr(&self) -> Option<u32> {
        let word = self.timecode_bcd()?;
        let phase = (word >> 31) & 1;
        Some((word & 0x7fff_ffff) | (phase << 15))
    }

    fn rate(&self) -> Option<f32> {
        self.fps.map(|(n, d)| (f64::from(n) / f64::from(d.max(1))) as f32)
    }
}

/// Scanlines in one ZIP block (OpenEXR's ZIP compression, as opposed to ZIPS with one).
const ZIP_LINES: usize = 16;

/// An OpenEXR file of `width × height` RGB pixels: `rgb` linear light, 3 floats a pixel, row by row.
/// Half floats (what grading software expects of a frame sequence), ZIP compressed.
///
/// `None` when `rgb` is not `width × height × 3` samples or a size is 0.
#[must_use]
pub fn encode_exr(width: usize, height: usize, rgb: &[f32], chroma: Chromaticities) -> Option<Vec<u8>> {
    encode_exr_with(width, height, rgb, chroma, Timing::default())
}

/// [`encode_exr`] with the frame rate (`framesPerSecond`) and timecode (`timeCode`) of `timing`.
#[must_use]
pub fn encode_exr_with(
    width: usize,
    height: usize,
    rgb: &[f32],
    chroma: Chromaticities,
    timing: Timing,
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width.checked_mul(height)?.checked_mul(3)? != rgb.len() {
        return None;
    }
    let (w, h) = (i32::try_from(width).ok()?, i32::try_from(height).ok()?);
    let mut out = vec![0x76, 0x2f, 0x31, 0x01, 2, 0, 0, 0];
    let mut attr = |name: &str, kind: &str, value: &[u8]| {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(kind.as_bytes());
        out.push(0);
        out.extend_from_slice(&(value.len() as u32).to_le_bytes());
        out.extend_from_slice(value);
    };
    // Channels in alphabetical order, as the format requires: half (1), linear, sampled 1 × 1.
    let mut channels = Vec::new();
    for name in ["B", "G", "R"] {
        channels.extend_from_slice(name.as_bytes());
        channels.push(0);
        channels.extend_from_slice(&1i32.to_le_bytes());
        channels.extend_from_slice(&[0, 0, 0, 0]);
        channels.extend_from_slice(&1i32.to_le_bytes());
        channels.extend_from_slice(&1i32.to_le_bytes());
    }
    channels.push(0);
    attr("channels", "chlist", &channels);
    attr("compression", "compression", &[3]);
    let window: Vec<u8> = [0, 0, w - 1, h - 1]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    attr("dataWindow", "box2i", &window);
    attr("displayWindow", "box2i", &window);
    attr("lineOrder", "lineOrder", &[0]);
    attr("pixelAspectRatio", "float", &1f32.to_le_bytes());
    attr("screenWindowCenter", "v2f", &[0; 8]);
    attr("screenWindowWidth", "float", &1f32.to_le_bytes());
    let chroma: Vec<u8> = chroma.0.iter().flat_map(|v| v.to_le_bytes()).collect();
    attr("chromaticities", "chromaticities", &chroma);
    if let Some((n, d)) = timing.fps {
        let mut v = i32::try_from(n).ok()?.to_le_bytes().to_vec();
        v.extend_from_slice(&d.to_le_bytes());
        attr("framesPerSecond", "rational", &v);
    }
    if let Some(tc) = timing.timecode_exr() {
        let mut v = tc.to_le_bytes().to_vec();
        v.extend_from_slice(&0u32.to_le_bytes());
        attr("timeCode", "timecode", &v);
    }
    out.push(0);

    // The blocks: each 16 scanlines, every scanline its channels one after another (B, G, R).
    let blocks = height.div_ceil(ZIP_LINES);
    let table_at = out.len();
    out.resize(table_at + blocks * 8, 0);
    let mut offsets = Vec::with_capacity(blocks);
    let mut raw = Vec::with_capacity(width * ZIP_LINES * 6);
    for (b, rows) in rgb.chunks(width * 3 * ZIP_LINES).enumerate() {
        raw.clear();
        for row in rows.chunks(width * 3) {
            for c in [2usize, 1, 0] {
                for px in row.as_chunks::<3>().0 {
                    let v = px.get(c).copied().unwrap_or(0.0);
                    let v = if v.is_finite() { v } else { 0.0 };
                    raw.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
                }
            }
        }
        let data = zip_block(&raw);
        offsets.push(out.len() as u64);
        out.extend_from_slice(&i32::try_from(b * ZIP_LINES).ok()?.to_le_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).ok()?.to_le_bytes());
        out.extend_from_slice(&data);
    }
    let table = out.get_mut(table_at..table_at + blocks * 8)?;
    for (slot, o) in table.as_chunks_mut::<8>().0.iter_mut().zip(offsets) {
        *slot = o.to_le_bytes();
    }
    Some(out)
}

/// One block's bytes as OpenEXR's ZIP compression stores them: the bytes split into the first and
/// second byte of each pair, then each byte as the difference to the one before, then deflated (zlib).
/// Stored raw when that is not smaller.
fn zip_block(raw: &[u8]) -> Vec<u8> {
    let half = raw.len().div_ceil(2);
    let mut t = vec![0u8; raw.len()];
    let (first, second) = t.split_at_mut(half);
    for (i, &v) in raw.iter().enumerate() {
        let slot = if i % 2 == 0 {
            first.get_mut(i / 2)
        } else {
            second.get_mut(i / 2)
        };
        if let Some(s) = slot {
            *s = v;
        }
    }
    let mut prev = t.first().copied().unwrap_or(0);
    for v in t.iter_mut().skip(1) {
        let here = *v;
        *v = here.wrapping_sub(prev).wrapping_add(128);
        prev = here;
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(4));
    match z.write_all(&t).and_then(|()| z.finish()) {
        Ok(packed) if packed.len() < raw.len() => packed,
        _ => raw.to_vec(),
    }
}

/// Whether `bytes` is a whole OpenEXR file of `width × height` as [`encode_exr`] writes it: its
/// last block ends where the file does (a file cut short does not).
#[must_use]
pub fn exr_complete(bytes: &[u8], width: usize, height: usize) -> bool {
    let Some(start) = exr_last_block(bytes, width, height) else {
        return false;
    };
    let Some(&[a, b, c, d]) = bytes.get(start + 4..start + 8) else {
        return false;
    };
    start + 8 + u32::from_le_bytes([a, b, c, d]) as usize == bytes.len()
}

/// The width and height an OpenEXR file's `dataWindow` names (`None` when its header is unreadable).
#[must_use]
pub fn exr_size(bytes: &[u8]) -> Option<(usize, usize)> {
    if !bytes.starts_with(&[0x76, 0x2f, 0x31, 0x01]) {
        return None;
    }
    let mut at = 8usize;
    for _ in 0..64 {
        let rest = bytes.get(at..)?;
        if rest.first() == Some(&0) {
            return None;
        }
        let name_end = rest.iter().position(|&b| b == 0)?;
        let kind_end = rest
            .get(name_end + 1..)
            .and_then(|r| r.iter().position(|&b| b == 0))?;
        let size_at = at + name_end + 1 + kind_end + 1;
        let &[a, b, c, d] = bytes.get(size_at..size_at + 4)? else {
            return None;
        };
        let size = u32::from_le_bytes([a, b, c, d]) as usize;
        if rest.get(..name_end) == Some(b"dataWindow".as_slice()) {
            let v = bytes.get(size_at + 4..size_at + 20)?;
            let n = |i: usize| -> Option<i64> {
                let &[a, b, c, d] = v.get(i * 4..i * 4 + 4)? else {
                    return None;
                };
                Some(i64::from(i32::from_le_bytes([a, b, c, d])))
            };
            let w = usize::try_from(n(2)? - n(0)? + 1).ok()?;
            let h = usize::try_from(n(3)? - n(1)? + 1).ok()?;
            return Some((w, h));
        }
        at = size_at.checked_add(4)?.checked_add(size)?;
    }
    None
}

/// Where the last block of an OpenEXR file of `width × height` begins, from its header and offset
/// table (`bytes` needs only the file's beginning, through its offset table).
#[must_use]
pub fn exr_last_block(bytes: &[u8], width: usize, height: usize) -> Option<usize> {
    if !bytes.starts_with(&[0x76, 0x2f, 0x31, 0x01]) || width == 0 {
        return None;
    }
    // The attributes: name, type, size, value; an empty name ends them.
    let mut at = 8usize;
    for _ in 0..64 {
        let rest = bytes.get(at..)?;
        if rest.first() == Some(&0) {
            at += 1;
            break;
        }
        let name_end = rest.iter().position(|&b| b == 0)?;
        let kind_end = rest
            .get(name_end + 1..)
            .and_then(|r| r.iter().position(|&b| b == 0))?;
        let size_at = at + name_end + 1 + kind_end + 1;
        let &[a, b, c, d] = bytes.get(size_at..size_at + 4)? else {
            return None;
        };
        at = size_at + 4 + u32::from_le_bytes([a, b, c, d]) as usize;
    }
    let blocks = height.div_ceil(ZIP_LINES);
    let &last = bytes
        .get(at..at + blocks * 8)
        .and_then(|t| t.as_chunks::<8>().0.last())?;
    usize::try_from(u64::from_le_bytes(last)).ok()
}

/// How a DPX file's values are encoded (its header's transfer and colorimetric codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpxTransfer {
    /// ITU-R BT.709 (code 6): Rec. 709 frames.
    Rec709,
    /// sRGB: transfer user defined (code 0, ST 268 has no sRGB curve), colorimetric BT.709 (code 6),
    /// as sRGB shares its primaries.
    Srgb,
    /// User defined (code 0): sRGB, PQ and HLG, which ST 268 has no code for in its first edition;
    /// the file's description names it.
    Other,
}

/// Size of a DPX header: file, image, orientation and film/TV information.
const DPX_HEADER: usize = 2048;

/// A DPX file of `width × height` RGB pixels: `rgb` 16-bit values (3 a pixel, row by row), stored as
/// 10 bits (method A: three in each 32-bit word), big-endian. `description` (up to 31 bytes) says
/// what the values are. `None` on a size that does not match `rgb`.
#[must_use]
pub fn encode_dpx(
    width: usize,
    height: usize,
    rgb: &[u16],
    transfer: DpxTransfer,
    description: &str,
) -> Option<Vec<u8>> {
    encode_dpx_with(width, height, rgb, transfer, description, Timing::default(), "")
}

/// [`encode_dpx`] with the frame rate, frame number and timecode of `timing` in the film and
/// television headers, and `file` (the frame's file name) in the file header. No time stamp:
/// two renders of the same frames are the same to the byte.
#[must_use]
pub fn encode_dpx_with(
    width: usize,
    height: usize,
    rgb: &[u16],
    transfer: DpxTransfer,
    description: &str,
    timing: Timing,
    file: &str,
) -> Option<Vec<u8>> {
    let pixels = width.checked_mul(height)?;
    if width == 0 || height == 0 || pixels.checked_mul(3)? != rgb.len() {
        return None;
    }
    let size = u32::try_from(DPX_HEADER.checked_add(pixels.checked_mul(4)?)?).ok()?;
    let mut out = vec![0u8; DPX_HEADER];
    let mut put = |at: usize, bytes: &[u8]| {
        if let Some(slot) = out.get_mut(at..at + bytes.len()) {
            slot.copy_from_slice(bytes);
        }
    };
    let text = |s: &str, n: usize| {
        let mut b: Vec<u8> = s.bytes().take(n - 1).collect();
        b.resize(n, 0);
        b
    };
    // File information.
    put(0, b"SDPX");
    put(4, &(DPX_HEADER as u32).to_be_bytes());
    put(8, b"V2.0\0\0\0\0");
    put(16, &size.to_be_bytes());
    put(20, &1u32.to_be_bytes());
    put(24, &1664u32.to_be_bytes());
    put(28, &384u32.to_be_bytes());
    put(32, &0u32.to_be_bytes());
    // The frame's own file name (a conform or an archive reads it), and what made it.
    // ASCII, as the header's text fields are (anything else as "_").
    let ascii: String = file
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '_'
            }
        })
        .collect();
    put(36, &text(&ascii, 100));
    put(
        160,
        &text(&format!("Lapsewerk {}", env!("CARGO_PKG_VERSION")), 100),
    );
    put(660, &u32::MAX.to_be_bytes());
    // Image information: one element, RGB.
    put(768, &0u16.to_be_bytes());
    put(770, &1u16.to_be_bytes());
    put(772, &u32::try_from(width).ok()?.to_be_bytes());
    put(776, &u32::try_from(height).ok()?.to_be_bytes());
    let e = 780;
    put(e, &0u32.to_be_bytes());
    put(e + 4, &0u32.to_be_bytes());
    put(e + 8, &0f32.to_be_bytes());
    put(e + 12, &1023u32.to_be_bytes());
    put(e + 16, &0f32.to_be_bytes());
    let (curve, primaries) = match transfer {
        DpxTransfer::Rec709 => (6, 6),
        DpxTransfer::Srgb => (0, 6),
        DpxTransfer::Other => (0, 0),
    };
    put(e + 20, &[50, curve, primaries, 10]);
    put(e + 24, &1u16.to_be_bytes());
    put(e + 26, &0u16.to_be_bytes());
    put(e + 28, &(DPX_HEADER as u32).to_be_bytes());
    put(e + 40, &text(description, 32));
    // The other seven elements: undefined.
    for k in 1..8 {
        put(e + k * 72, &[0xff; 72]);
    }
    // Orientation header: square pixels (an editor reads 0:0 as unknown).
    put(1628, &1u32.to_be_bytes());
    put(1632, &1u32.to_be_bytes());
    // Film and television headers: undefined (all bits set) unless known.
    put(1664, &[0xff; 384]);
    put(1664, &[0; 32]);
    put(1680, &[0; 32]);
    put(1712, &(timing.index + 1).to_be_bytes());
    put(1732, &[0; 132]);
    if let Some(rate) = timing.rate() {
        put(1724, &rate.to_be_bytes());
        put(1940, &rate.to_be_bytes());
    }
    put(1920, &timing.timecode_bcd().unwrap_or(u32::MAX).to_be_bytes());
    out.reserve(pixels * 4);
    for &[r, g, b] in rgb.as_chunks::<3>().0 {
        // Rounded to the nearest of the 1024 codes (cutting off the low bits made every value
        // up to a code darker).
        let ten = |v: u16| (u32::from(v) * 1023 + 32_767) / 65_535;
        out.extend_from_slice(&((ten(r) << 22) | (ten(g) << 12) | (ten(b) << 2)).to_be_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lw-still-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// ffmpeg's reading of `path` as 16-bit RGB (`None` without ffmpeg).
    fn ffmpeg_rgb48(path: &std::path::Path, pix: &str) -> Option<Vec<u16>> {
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-f", "rawvideo", "-pix_fmt", pix, "-"])
            .output()
            .ok()?;
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        Some(
            out.stdout
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect(),
        )
    }

    #[test]
    fn exr_frames_hold_their_light_and_read_back() {
        // 37 × 21: two ZIP blocks, the second short; a ramp up to 4 (HDR headroom) in red.
        let (w, h) = (37, 21);
        let mut rgb = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&[x as f32 / (w - 1) as f32 * 4.0, y as f32 / (h - 1) as f32, 0.25]);
            }
        }
        let exr = encode_exr_with(
            w,
            h,
            &rgb,
            Chromaticities::REC2020,
            Timing {
                fps: Some((30_000, 1001)),
                index: 0,
                start: Some(0),
                drop: false,
            },
        )
        .unwrap();
        assert!(exr.windows(15).any(|w| w == b"framesPerSecond"));
        assert!(exr.windows(8).any(|w| w == b"timeCode"));
        assert!(exr_complete(&exr, w, h));
        assert!(!exr_complete(&exr[..exr.len() - 1], w, h));
        assert!(!exr_complete(&exr[..exr.len() / 2], w, h));
        assert!(encode_exr(w, h, &rgb[3..], Chromaticities::REC709).is_none());
        let path = dir().join("f.exr");
        std::fs::write(&path, &exr).unwrap();
        // ffmpeg reads the half floats as they are (gbrpf16le: converted to 32-bit floats, ffmpeg 9
        // clips at 1 and gives 0 back as 1e-4); turned into 32-bit floats here.
        let Ok(out) = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "rawvideo", "-pix_fmt", "gbrpf16le", "-"])
            .output()
        else {
            eprintln!("skipped: no ffmpeg");
            return;
        };
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let half = |b: [u8; 2]| -> f32 {
            let v = u16::from_le_bytes(b);
            let sign = if v & 0x8000 == 0 { 1.0 } else { -1.0 };
            let (e, m) = (i32::from((v >> 10) & 0x1f), f32::from(v & 0x3ff));
            sign * match e {
                0 => m * 2f32.powi(-24),
                31 => f32::INFINITY,
                _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
            }
        };
        let f: Vec<f32> = out.stdout.as_chunks::<2>().0.iter().map(|b| half(*b)).collect();
        assert_eq!(f.len(), w * h * 3);
        // Planar G, B, R.
        let n = w * h;
        for (i, px) in rgb.chunks(3).enumerate() {
            let (g, b, r) = (f[i], f[n + i], f[2 * n + i]);
            for (got, want) in [(r, px[0]), (g, px[1]), (b, px[2])] {
                assert!(
                    (got - want).abs() <= want.abs() / 1000.0 + 1e-4,
                    "{i}: {got} vs {want}"
                );
            }
        }
    }

    #[test]
    fn dpx_frames_hold_ten_bits_and_read_back() {
        let (w, h) = (20, 6);
        let rgb: Vec<u16> = (0..w * h * 3).map(|i| (i * 547 % 65536) as u16).collect();
        // Frame 3 of a sequence at 25 fps starting at 01:00:00:00: 01:00:00:03.
        let timing = Timing {
            fps: Some((25, 1)),
            index: 3,
            start: Some(90_000),
            drop: false,
        };
        assert_eq!(timing.timecode_bcd(), Some(0x0100_0003));
        // 50 frames a second: frame 41 of a second is pair 20, second of its pair; no drop flag.
        let fifty = Timing {
            fps: Some((50, 1)),
            index: 41,
            start: Some(0),
            drop: false,
        };
        let word = fifty.timecode_bcd().unwrap();
        assert_eq!(word & 0x3f, 0x20, "{word:08x}");
        assert_eq!(word & 0x40, 0, "no drop-frame flag");
        assert_eq!(
            word & 0x8000_0000,
            0x8000_0000,
            "second of the pair (25-frame system)"
        );
        assert_eq!(fifty.timecode_exr().unwrap() & 0x8000, 0x8000, "OpenEXR: bit 15");
        let sixty = Timing {
            fps: Some((60, 1)),
            ..fifty
        };
        assert_eq!(
            sixty.timecode_bcd().unwrap() & 0x8000,
            0x8000,
            "30-frame system: bit 15"
        );
        assert_eq!(
            Timing {
                fps: Some((120, 1)),
                ..fifty
            }
            .timecode_bcd(),
            None
        );
        assert_eq!(
            Timing {
                index: 25 * 61 + 7,
                ..timing
            }
            .timecode_bcd(),
            Some(0x0101_0107)
        );
        let dpx = encode_dpx_with(
            w,
            h,
            &rgb,
            DpxTransfer::Rec709,
            "Rec. 709",
            timing,
            "f_000004.dpx",
        )
        .unwrap();
        assert_eq!(&dpx[36..48], b"f_000004.dpx");
        assert_eq!(dpx[1920..1924], [0x01, 0x00, 0x00, 0x03]);
        assert_eq!(f32::from_be_bytes(dpx[1724..1728].try_into().unwrap()), 25.0);
        assert_eq!(dpx.len(), 2048 + w * h * 4);
        assert_eq!(dpx[800..803], [50, 6, 6]);
        assert_eq!(dpx[1628..1636], [0, 0, 0, 1, 0, 0, 0, 1]);
        let srgb = encode_dpx(w, h, &rgb, DpxTransfer::Srgb, "sRGB").unwrap();
        assert_eq!(srgb[800..803], [50, 0, 6]);
        assert!(encode_dpx(w, h + 1, &rgb, DpxTransfer::Other, "").is_none());
        let path = dir().join("f.dpx");
        std::fs::write(&path, &dpx).unwrap();
        let Some(back) = ffmpeg_rgb48(&path, "rgb48le") else {
            eprintln!("skipped: no ffmpeg");
            return;
        };
        assert_eq!(back.len(), rgb.len());
        for (got, want) in back.iter().zip(&rgb) {
            // 10 bits (the nearest code), scaled back to 16 by ffmpeg.
            let code = (u32::from(*want) * 1023 + 32_767) / 65_535;
            assert_eq!(u32::from(got >> 6), code, "{got} vs {want}");
        }
    }
}

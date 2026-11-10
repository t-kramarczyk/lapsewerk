//! Hot and stuck pixels: single sensor samples far brighter than their surroundings in frame after
//! frame, found in the raw mosaic (before demosaicing spreads them into coloured blots).
//!
//! In one frame, a star or a distant lamp can look like a hot pixel. Three tests tell them apart:
//! - **Flat surroundings.** The same-colour samples around it agree with each other (in scene detail,
//!   such as a city at night, they do not, and a coloured lamp lifts one colour only).
//! - **Isolation.** Light from the scene passes through the lens, which spreads even a star over
//!   more than one sample, so the samples right beside it (other colours) are lifted too. A hot
//!   pixel is a fault of one sample: its neighbours are as dark as the ring around them.
//! - **Persistence.** Stars move between frames taken minutes apart; a hot pixel stays on the same
//!   sample. Only samples flagged in most of the frames checked count ([`persistent`]).

use std::collections::HashMap;

/// A single-sample-per-pixel raw mosaic.
pub struct Mosaic<'a> {
    pub width: usize,
    pub height: usize,
    pub data: &'a [u16],
    /// Colour index (0 red, 1 green, 2 blue) of the sample at (x, y).
    pub colour: &'a (dyn Fn(usize, usize) -> u8 + Sync),
    /// Black and white level (raw units).
    pub black: f32,
    pub white: f32,
    /// The part of the mosaic holding image data: x, y, width, height.
    pub area: (usize, usize, usize, usize),
}

impl Mosaic<'_> {
    fn at(&self, x: usize, y: usize) -> f32 {
        self.data.get(y * self.width + x).map_or(0.0, |v| f32::from(*v))
    }
}

/// Positions (`y · width + x`) of samples that stand alone above their surroundings by more than
/// `min_excess` of the sensor's range in this frame.
#[must_use]
pub fn spikes(m: &Mosaic<'_>, min_excess: f32) -> Vec<usize> {
    let range = m.white - m.black;
    if range.is_nan() || range <= 0.0 || m.data.len() < m.width * m.height {
        return Vec::new();
    }
    let (ax, ay, aw, ah) = m.area;
    // 2 samples from the edge of the image area: the 5 × 5 neighbourhood stays inside it.
    if aw < 8 || ah < 8 || ax + aw > m.width || ay + ah > m.height {
        return Vec::new();
    }
    let threshold = min_excess * range;
    let rows: Vec<usize> = (ay + 2..ay + ah - 2).collect();
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let per = rows.len().div_ceil(threads).max(1);
    let mut found = Vec::new();
    std::thread::scope(|s| {
        let handles: Vec<_> = rows
            .chunks(per)
            .map(|chunk| {
                s.spawn(move || {
                    let mut out = Vec::new();
                    for &y in chunk {
                        for x in ax + 2..ax + aw - 2 {
                            if let Some(i) = check(m, x, y, threshold) {
                                out.push(i);
                            }
                        }
                    }
                    out
                })
            })
            .collect();
        for h in handles {
            if let Ok(v) = h.join() {
                found.extend(v);
            }
        }
    });
    found.sort_unstable();
    found
}

/// The sample at (x, y) if it is a lone spike.
fn check(m: &Mosaic<'_>, x: usize, y: usize, threshold: f32) -> Option<usize> {
    let v = m.at(x, y);
    // Cheap first test: above every sample around it by the threshold.
    let mut around = 0.0f32;
    for (dx, dy) in [
        (-1isize, -1isize),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ] {
        around = around.max(m.at(x.wrapping_add_signed(dx), y.wrapping_add_signed(dy)));
    }
    if v - around < threshold {
        return None;
    }
    // Same-colour neighbours in the 5 × 5 neighbourhood: the level the sample should have.
    let colour = (m.colour)(x, y);
    let mut same = Vec::with_capacity(12);
    let mut ring = (0.0f32, 0usize);
    for dy in -2isize..=2 {
        for dx in -2isize..=2 {
            if (dx, dy) == (0, 0) {
                continue;
            }
            let (nx, ny) = (x.wrapping_add_signed(dx), y.wrapping_add_signed(dy));
            let n = m.at(nx, ny);
            if (m.colour)(nx, ny) == colour {
                same.push(n);
            }
            if dx.abs() == 2 || dy.abs() == 2 {
                ring.0 += n;
                ring.1 += 1;
            }
        }
    }
    if same.len() < 4 {
        return None;
    }
    same.sort_by(f32::total_cmp);
    let level = same.get(same.len() / 2).copied().unwrap_or(0.0);
    let excess = v - level;
    if excess < threshold {
        return None;
    }
    // Flat surroundings: the same-colour neighbours agree with each other. Scene detail (a city at
    // night, where a coloured lamp lifts one colour only and passes the isolation test below) does
    // not; a hot pixel there would not show anyway.
    let spread = same.last().copied().unwrap_or(0.0) - same.first().copied().unwrap_or(0.0);
    if spread > 0.2 * excess {
        return None;
    }
    // Isolation: the four samples touching it are not lifted above the ring around them (scene
    // light, spread by the lens, would lift them).
    let adjacent = (m.at(x - 1, y) + m.at(x + 1, y) + m.at(x, y - 1) + m.at(x, y + 1)) / 4.0;
    let ring_mean = ring.0 / ring.1.max(1) as f32;
    if adjacent - ring_mean > 0.15 * excess {
        return None;
    }
    Some(y * m.width + x)
}

/// Samples flagged in at least `min_fraction` of the frames' lists (and in at least two).
#[must_use]
pub fn persistent(lists: &[Vec<usize>], min_fraction: f64) -> Vec<usize> {
    let mut count: HashMap<usize, usize> = HashMap::new();
    for list in lists {
        for &i in list {
            *count.entry(i).or_default() += 1;
        }
    }
    let need = ((lists.len() as f64 * min_fraction).ceil() as usize).max(2);
    let mut out: Vec<usize> = count
        .into_iter()
        .filter(|(_, c)| *c >= need)
        .map(|(i, _)| i)
        .collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 40;
    const H: usize = 30;

    fn rggb(x: usize, y: usize) -> u8 {
        match (x % 2, y % 2) {
            (0, 0) => 0,
            (1, 1) => 2,
            _ => 1,
        }
    }

    /// A dark frame (black 512, a little texture) with a hot pixel at (10, 10), a star (a lens-spread
    /// blob, peak at (`sx`, 20)) and a red lamp in a lit, uneven patch (red samples only, at (30, 10)).
    fn frame(sx: usize) -> Vec<u16> {
        let mut d: Vec<u16> = (0..W * H).map(|i| 600 + ((i * 7919) % 23) as u16).collect();
        d[10 * W + 10] = 4000;
        for dy in -1isize..=1 {
            for dx in -1isize..=1 {
                let (x, y) = (sx.wrapping_add_signed(dx), 20usize.wrapping_add_signed(dy));
                let lift = if (dx, dy) == (0, 0) { 6000 } else { 2500 };
                d[y * W + x] += lift;
            }
        }
        // A lit patch of red samples of different brightness, the lamp at its centre.
        for (k, (x, y)) in [
            (28, 8),
            (30, 8),
            (32, 8),
            (28, 10),
            (32, 10),
            (28, 12),
            (30, 12),
            (32, 12),
        ]
        .into_iter()
        .enumerate()
        {
            d[y * W + x] = 1500 + 400 * k as u16;
        }
        d[10 * W + 30] = 9000;
        d
    }

    fn mosaic(data: &[u16]) -> Mosaic<'_> {
        Mosaic {
            width: W,
            height: H,
            data,
            colour: &rggb,
            black: 512.0,
            white: 16383.0,
            area: (0, 0, W, H),
        }
    }

    #[test]
    fn finds_the_hot_pixel_not_the_star() {
        let d = frame(25);
        assert_eq!(spikes(&mosaic(&d), 0.03), vec![10 * W + 10]);
        // Below the threshold: nothing.
        assert!(spikes(&mosaic(&d), 0.5).is_empty());
    }

    #[test]
    fn a_lone_star_sample_is_dropped_by_persistence() {
        // Suppose a faint star were sharp enough to pass the isolation test: it moves.
        let lists = vec![vec![5, 100], vec![5, 140], vec![5, 180], vec![5]];
        assert_eq!(persistent(&lists, 0.5), vec![5]);
        assert!(
            persistent(&[vec![7]], 0.5).is_empty(),
            "one frame is never enough"
        );
    }

    #[test]
    fn degenerate_mosaics_find_nothing() {
        let d = frame(25);
        let mut m = mosaic(&d);
        m.white = m.black;
        assert!(spikes(&m, 0.03).is_empty());
        let mut m = mosaic(&d);
        m.area = (0, 0, W + 1, H);
        assert!(spikes(&m, 0.03).is_empty());
        let short = vec![0u16; 10];
        assert!(spikes(&mosaic(&short), 0.03).is_empty());
    }
}

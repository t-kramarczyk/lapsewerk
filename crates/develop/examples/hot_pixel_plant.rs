//! Plants hot pixels into real raw frames (the same samples in every frame, at chosen strengths) and
//! checks that the sequence detection finds exactly them: recall and false finds on real scene data.
//!
//! `cargo run --release -p lapsewerk-develop --example hot_pixel_plant -- <frame> <frame> …`

use std::error::Error;
use std::path::Path;

use lapsewerk_core::defects::{self, Mosaic};

fn main() -> Result<(), Box<dyn Error>> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    let mut planted: Vec<(usize, f32)> = Vec::new();
    let mut lists = Vec::new();
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    for (k, p) in paths.iter().enumerate() {
        let bytes = std::fs::read(Path::new(p))?;
        let raw = lightcraft_raw::decode(&bytes)?;
        let lightcraft_raw::RawData::U16(data) = &raw.data else {
            return Err("not 16-bit".into());
        };
        let mut data = data.clone();
        let a = raw.active_area;
        let (black, white) = (raw.black.mean(), raw.white.iter().copied().fold(0.0, f32::max));
        let range = white - black;
        if k == 0 {
            // 300 positions over the whole image (sky, city, forest), strengths 1 %-20 % of the range.
            for n in 0..300 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let x = a.x + 4 + (seed % (a.width as u64 - 8)) as usize;
                let y = a.y + 4 + ((seed >> 32) % (a.height as u64 - 8)) as usize;
                let strength = [0.01f32, 0.03, 0.05, 0.1, 0.2].get(n % 5).copied().unwrap_or(0.1);
                planted.push((y * raw.width + x, strength));
            }
        }
        for &(i, strength) in &planted {
            if let Some(v) = data.get_mut(i) {
                *v = (f32::from(*v) + strength * range).min(65535.0) as u16;
            }
        }
        let cfa = raw.cfa.as_ref().ok_or("no CFA")?;
        let colour = |x: usize, y: usize| cfa.color_at(x, y);
        let m = Mosaic {
            width: raw.width,
            height: raw.height,
            data: &data,
            colour: &colour,
            black,
            white,
            area: (a.x, a.y, a.width, a.height),
        };
        lists.push(defects::spikes(&m, 0.02));
    }
    let found = defects::persistent(&lists, 0.5);
    let is_planted = |i: &usize| planted.iter().any(|(p, _)| p == i);
    let false_finds = found.iter().filter(|i| !is_planted(i)).count();
    println!(
        "frames {}, planted {}, found {}, false finds {false_finds}",
        paths.len(),
        planted.len(),
        found.len()
    );
    for strength in [0.01f32, 0.03, 0.05, 0.1, 0.2] {
        let of: Vec<usize> = planted
            .iter()
            .filter(|(_, s)| *s == strength)
            .map(|(i, _)| *i)
            .collect();
        let hit = of.iter().filter(|i| found.contains(i)).count();
        println!(
            "  strength {:>4.0} % of range: found {hit} of {}",
            strength * 100.0,
            of.len()
        );
    }
    Ok(())
}

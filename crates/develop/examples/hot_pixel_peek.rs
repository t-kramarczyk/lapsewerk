//! Prints the raw 5 × 5 neighbourhood of hot pixels found in a sequence, in two frames, to check by eye
//! that they are lone samples (and not stars or lamps).
//!
//! `cargo run --release -p lapsewerk-develop --example hot_pixel_peek -- <frame> <frame> [<frame> …]`

use std::error::Error;
use std::path::Path;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths: Vec<&Path> = args.iter().map(Path::new).collect();
    let (defects, checked) = lapsewerk_develop::hot_pixels(&paths, 0.02);
    println!("{} hot pixels in {checked} frames", defects.len());
    let mut singles = Vec::new();
    for p in &paths {
        let (_, _, s) = lapsewerk_develop::raw_spikes(p, 0.02).ok_or("not a raw")?;
        println!("  {}: {} lone spikes", p.display(), s.len());
        singles.extend(s.into_iter().take(2));
    }
    let defects = if defects.is_empty() { singles } else { defects };
    for path in paths.iter().take(2) {
        let bytes = std::fs::read(path)?;
        let raw = lightcraft_raw::decode(&bytes)?;
        let lightcraft_raw::RawData::U16(data) = &raw.data else {
            return Err("not 16-bit".into());
        };
        println!("== {}", path.display());
        let step = (defects.len() / 4).max(1);
        for &i in defects.iter().step_by(step).take(4) {
            let (x, y) = (i % raw.width, i / raw.width);
            println!("({x}, {y}):");
            for dy in -2isize..=2 {
                let row: Vec<String> = (-2isize..=2)
                    .map(|dx| {
                        let j = (y as isize + dy) as usize * raw.width + (x as isize + dx) as usize;
                        format!("{:>6}", data.get(j).copied().unwrap_or(0))
                    })
                    .collect();
                println!("  {}", row.join(""));
            }
        }
    }
    Ok(())
}

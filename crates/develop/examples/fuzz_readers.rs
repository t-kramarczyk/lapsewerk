//! Robustness check of the file readers Lapsewerk runs on every frame and on its own files: feeds
//! mutated copies of small real files to them and reports any panic or hang.
//!
//! `cargo run --release -p lapsewerk-develop --example fuzz_readers -- [ITERATIONS] [SEED] [--seeds DIR]`
//!
//! `--seeds DIR` adds every file in DIR as a seed: put a few real raws there (ARW, CR3, NEF …, the
//! smaller the better) to test the raw decoder itself, which the built-in seeds barely reach. A raw
//! seed costs a full decode per input: use fewer iterations (2000 take some minutes).
//!
//! Readers: EXIF (`exact_exposure`, i.e. `lightcraft_meta::try_extract`), raw probing and decoding
//! (`lightcraft_raw`, what `raw_brightness`, `raw_spikes` and hot pixel search use), the capture
//! time parser and the project file. Seeds: a JPEG and a TIFF with camera EXIF (a raw such as ARW is
//! a TIFF), and a project file. Mutations: bit flips, bytes set to edge values, 16/32-bit values
//! (sizes, offsets, counts) set to extremes, truncation, a block repeated. Deterministic for a seed.
//! A crashing input is written to the system temp folder for a regression test.
//!
//! Run it under a memory limit (`ulimit -v 4000000`) to see readers that trust sizes in a header.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::{Duration, Instant};

/// xorshift64*: small, deterministic, good enough to pick mutations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..=rng.below(4) {
        if b.is_empty() {
            break;
        }
        let at = rng.below(b.len());
        match rng.below(6) {
            0 => b[at] ^= 1 << rng.below(8),
            1 => b[at] = [0x00, 0xff, 0x7f, 0x80, 0x01][rng.below(5)],
            2 => {
                let v: u16 = [0, 1, 0xffff, 0x7fff, 0x8000][rng.below(5)];
                for (k, byte) in v.to_le_bytes().into_iter().enumerate() {
                    if let Some(x) = b.get_mut(at + k) {
                        *x = byte;
                    }
                }
            }
            3 => {
                let v: u32 = [0, 1, u32::MAX, 0x7fff_ffff, 0x8000_0000, 0x0010_0000][rng.below(6)];
                let bytes = if rng.below(2) == 0 {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                };
                for (k, byte) in bytes.into_iter().enumerate() {
                    if let Some(x) = b.get_mut(at + k) {
                        *x = byte;
                    }
                }
            }
            4 => b.truncate(at),
            _ => {
                let len = rng.below(64).min(b.len() - at);
                let block = b[at..at + len].to_vec();
                b.splice(at..at, block);
            }
        }
    }
    b
}

fn random_text(rng: &mut Rng) -> String {
    const PARTS: &[&str] = &[
        "2025", "-", "09", "11", "T", " ", ":", "23", ".", "123456", "+02:00", "ä", "9", "x", "60", "00",
    ];
    (0..rng.below(12))
        .map(|_| PARTS[rng.below(PARTS.len())])
        .collect()
}

struct Findings {
    panics: usize,
    slowest: (Duration, String),
}

fn check(name: &str, input: &[u8], f: impl FnOnce(), found: &mut Findings) {
    let started = Instant::now();
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        found.panics += 1;
        let path = std::env::temp_dir().join(format!("lapsewerk-fuzz-{name}-{}.bin", found.panics));
        let _ = std::fs::write(&path, input);
        eprintln!("PANIC in {name}: input saved to {}", path.display());
    }
    let took = started.elapsed();
    if took > found.slowest.0 {
        found.slowest = (took, name.to_owned());
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let extra_dir = args.iter().position(|a| a == "--seeds").map(|i| {
        let dir = args.get(i + 1).cloned().expect("--seeds needs a folder");
        args.drain(i..=i + 1);
        dir
    });
    let iterations: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(20_000);
    let mut rng = Rng(args
        .get(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(0x5eed_1234_abcd_ef01)
        | 1);
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let jpeg = std::fs::read(data.join("exif.jpg")).expect("seed exif.jpg");
    let tiff = std::fs::read(data.join("exif.tif")).expect("seed exif.tif");
    let project = br#"{"lapsewerk": 1, "intervalMs": 5000, "gapsBefore": [1], "outOfOrder": [], "exposureSteps": [{"frame": 1, "stops": 1.0}], "frames": [{"file": "a.arw", "capturedMs": 0, "shutterS": 0.01, "aperture": 8, "iso": 100, "keyframe": true}, {"file": "b.arw", "capturedMs": 5000}]}"#.to_vec();

    // The seeds must be read correctly, or mutating them tests nothing.
    let tmp = std::env::temp_dir().join(format!("lapsewerk-fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("temp folder");
    for (name, seed) in [("exif.jpg", &jpeg), ("exif.tif", &tiff)] {
        let p = tmp.join(name);
        std::fs::write(&p, seed).expect("write seed");
        let Some(e) = lapsewerk_develop::exact_exposure(&p) else {
            panic!("seed {name} has no EXIF exposure LightCraft reads")
        };
        assert!(
            (e.shutter_s - 1.0 / 250.0).abs() < 1e-9
                && (e.aperture - 8.0).abs() < 1e-9
                && (e.iso - 400.0).abs() < 1e-9
        );
    }
    assert!(
        lapsewerk_develop::project::Project::from_json(std::str::from_utf8(&project).unwrap(), &tmp).is_ok()
    );

    // Seeds of the user's: every file in the folder, checked like the built-in ones (a raw must decode).
    let mut extra: Vec<(String, Vec<u8>)> = Vec::new();
    if let Some(dir) = &extra_dir {
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .expect("--seeds folder")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && !p
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            })
            .collect();
        paths.sort();
        for p in paths {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let bytes = std::fs::read(&p).expect("read seed");
            let raw = lightcraft_raw::probe(&bytes).is_some();
            let decodes = raw && lightcraft_raw::decode(&bytes).is_ok();
            let exif = lapsewerk_develop::exact_exposure(&p).is_some();
            println!(
                "seed {name}: {} kB, {}, EXIF exposure {}",
                bytes.len() / 1024,
                if decodes {
                    "raw, decodes"
                } else if raw {
                    "raw, DOES NOT DECODE"
                } else {
                    "not a raw"
                },
                if exif { "read" } else { "missing" }
            );
            extra.push((name, bytes));
        }
        assert!(!extra.is_empty(), "no files in the --seeds folder");
    }

    let mut found = Findings {
        panics: 0,
        slowest: (Duration::ZERO, String::new()),
    };
    let file = tmp.join("mutant");
    let started = Instant::now();
    for i in 0..iterations {
        // With seeds of the user's, every other input is one of theirs.
        let kind = if extra.is_empty() {
            i % 3
        } else if i % 2 == 0 {
            3
        } else {
            (i / 2) % 3
        };
        let seed = match kind {
            0 => &jpeg,
            1 => &tiff,
            2 => &project,
            _ => &extra.get((i / 2) % extra.len().max(1)).expect("extra seed").1,
        };
        let input = mutate(seed, &mut rng);
        if kind == 2 {
            let text = String::from_utf8_lossy(&input).into_owned();
            check(
                "project",
                &input,
                || drop(lapsewerk_develop::project::Project::from_json(&text, &tmp)),
                &mut found,
            );
            let t = random_text(&mut rng);
            check(
                "capture_time",
                t.as_bytes(),
                || {
                    let _ = lapsewerk_develop::parse_local_ms(&t);
                },
                &mut found,
            );
            continue;
        }
        check(
            "meta",
            &input,
            || drop(lightcraft_meta::try_extract(&input)),
            &mut found,
        );
        std::fs::write(&file, &input).expect("write mutant");
        check(
            "exact_exposure",
            &input,
            || {
                let _ = lapsewerk_develop::exact_exposure(&file);
            },
            &mut found,
        );
        check(
            "raw",
            &input,
            || {
                if lightcraft_raw::probe(&input).is_some()
                    && let Ok(raw) = lightcraft_raw::decode(&input)
                {
                    drop(raw.normalized());
                }
            },
            &mut found,
        );
        check(
            "raw_brightness",
            &input,
            || {
                let _ = lapsewerk_develop::raw_brightness(&file);
            },
            &mut found,
        );
        check(
            "raw_spikes",
            &input,
            || drop(lapsewerk_develop::raw_spikes(&file, 0.02)),
            &mut found,
        );
        if (i + 1) % 5000 == 0 {
            eprintln!("{} inputs, {} panics", i + 1, found.panics);
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    println!(
        "{iterations} inputs in {:.1} s: {} panics; slowest single read {:.0} ms ({})",
        started.elapsed().as_secs_f64(),
        found.panics,
        found.slowest.0.as_secs_f64() * 1000.0,
        found.slowest.1
    );
    if found.panics > 0 {
        std::process::exit(1);
    }
}

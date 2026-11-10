//! Camera profiles from one's own photos: LightCraft's method (`lightcraft-cli calibrate`) pools the
//! colours of many raw files of one camera against the camera's own embedded JPEG and fits a colour
//! matrix and hue/saturation table per model. Raw files without colour matrices (Sony ARW, Nikon
//! NEF, Fujifilm RAF) are then developed with it, so colours match the camera's look from the first
//! frame on. Profiles go to LightCraft's profiles folder, where LightCraft and Lapsewerk read them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lightcraft_engine::camera_profiles::{self, Pool};

/// Fewest usable photos of a model for a profile.
pub const MIN_PHOTOS: usize = 5;

/// What a calibration made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Calibrated {
    /// Profiles written: model, photos used, file.
    pub written: Vec<(String, usize, PathBuf)>,
    /// Models with too few usable photos: model, photos.
    pub too_few: Vec<(String, usize)>,
    /// Files that could not contribute (no camera JPEG, unreadable, a format with its own matrices).
    pub skipped: usize,
}

/// Picks at most `max` of `files`, spread over all of them (dates, scenes), not the first ones.
#[must_use]
pub fn spread(files: &[PathBuf], max: usize) -> Vec<PathBuf> {
    let n = files.len();
    if max == 0 || n <= max {
        return files.to_vec();
    }
    (0..max).filter_map(|k| files.get(k * n / max).cloned()).collect()
}

/// Pools `files` into profiles and writes them to `dir` (else LightCraft's profiles folder).
/// `progress(done, total)` is called after every file; returning `false` stops (nothing written).
///
/// # Errors
/// When there is no profiles folder, a profile cannot be written, or the run was stopped.
pub fn calibrate(
    files: &[PathBuf],
    dir: Option<&Path>,
    progress: &(dyn Fn(usize, usize) -> bool + Sync),
) -> Result<Calibrated, String> {
    let dir = dir
        .map(Path::to_path_buf)
        .or_else(camera_profiles::dir)
        .ok_or("no folder for camera profiles")?;
    let total = files.len();
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 6);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let pools: Vec<Pool> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut pool = Pool::default();
                    while !stop.load(Ordering::Relaxed) {
                        let Some(path) = files.get(next.fetch_add(1, Ordering::Relaxed)) else {
                            break;
                        };
                        // A crash on one hostile file must not end the run.
                        let added = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            std::fs::read(path)
                                .map_err(|e| e.to_string())
                                .and_then(|b| pool.add(&b))
                        }));
                        if !matches!(added, Ok(Ok(Some(_)))) {
                            skipped.fetch_add(1, Ordering::Relaxed);
                        }
                        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                        if !progress(n, total) {
                            stop.store(true, Ordering::Relaxed);
                        }
                    }
                    pool
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    if stop.load(Ordering::Relaxed) {
        return Err("stopped".into());
    }
    let mut pool = Pool::default();
    for p in pools {
        pool.merge(p);
    }
    let mut out = Calibrated {
        skipped: skipped.load(Ordering::Relaxed),
        ..Calibrated::default()
    };
    // A model whose colours could not be fitted stays as it was.
    for profile in pool.fit(MIN_PHOTOS).into_iter().flatten() {
        let path = camera_profiles::save(&profile, &dir)?;
        out.written.push((profile.model.clone(), profile.files, path));
    }
    out.too_few = pool
        .files()
        .into_iter()
        .filter(|(_, n)| *n < MIN_PHOTOS)
        .collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreads_over_all_files() {
        let files: Vec<PathBuf> = (0..10).map(|i| PathBuf::from(format!("{i}.arw"))).collect();
        assert_eq!(spread(&files, 0).len(), 10);
        assert_eq!(spread(&files, 20).len(), 10);
        let s = spread(&files, 3);
        assert_eq!(
            s,
            [
                PathBuf::from("0.arw"),
                PathBuf::from("3.arw"),
                PathBuf::from("6.arw")
            ]
        );
    }

    #[test]
    fn files_without_a_camera_profile_are_skipped_and_a_stop_is_an_error() {
        let dir = std::env::temp_dir().join(format!("lapsewerk-calibrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jpeg = dir.join("a.jpg");
        std::fs::write(&jpeg, include_bytes!("../tests/data/frame.jpg")).unwrap();
        let junk = dir.join("b.arw");
        std::fs::write(&junk, b"not a raw file at all").unwrap();
        let files = vec![jpeg, junk, dir.join("missing.arw")];
        let out = calibrate(&files, Some(&dir.join("profiles")), &|_, _| true).unwrap();
        assert!(out.written.is_empty());
        assert_eq!(out.skipped, 3);
        assert!(calibrate(&files, Some(&dir.join("profiles")), &|_, _| false).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! The lens database (lensfun, CC BY-SA 3.0), whose profiles Lapsewerk uses to correct the distortion
//! of the lenses it lists (their vignetting only on the user's choice, see `Sequence::settings`): downloaded only on the user's click, into LightCraft's data folder, where LightCraft
//! reads it too. Nothing of it is bundled. While it is there, a raw whose lens it knows is opened
//! with its lens correction on (as LightCraft imports it), unless a keyframe turns it off.

use std::path::PathBuf;

use lightcraft_engine::activity::Activity;
use lightcraft_engine::lensfun_download::{self, Download, State};

/// Where the lens database stands on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Installed (the pinned data, complete): lens corrections apply.
    Installed,
    /// A folder of the user's own (`LIGHTCRAFT_LENSFUN_DIR`): used as it is, never downloaded over.
    Own,
    /// Not there: it can be downloaded into this folder.
    Missing(PathBuf),
    /// No data folder on this system: nothing to offer.
    Unavailable,
}

/// The lens database's status now.
#[must_use]
pub fn status() -> Status {
    if std::env::var_os(lightcraft_engine::lens_profiles::ENV).is_some_and(|v| !v.is_empty()) {
        return Status::Own;
    }
    match lensfun_download::default_dir() {
        Some(d) if lensfun_download::installed(&d) => Status::Installed,
        Some(d) => Status::Missing(d),
        None => Status::Unavailable,
    }
}

/// The lens database in use, for a render's settings: which data corrected the lenses, so a resume
/// after a download is refused for that reason (not for a keyframe that seems edited).
#[must_use]
pub fn words() -> String {
    match status() {
        Status::Installed => format!("lensfun {}", &lensfun_download::COMMIT[..7]),
        Status::Own => "a lens database of your own (LIGHTCRAFT_LENSFUN_DIR)".to_owned(),
        Status::Missing(_) | Status::Unavailable => "none".to_owned(),
    }
}

/// The download's size in whole megabytes ("about 5 MB").
#[must_use]
pub fn megabytes() -> u64 {
    lensfun_download::download_megabytes()
}

/// How a download goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Idle,
    Running { bytes: u64, total: u64 },
    Done,
    Failed(String),
}

/// A download of the lens database, started on the user's click (the background thread is
/// LightCraft's: pinned files, sizes and SHA-256 checked, put in place whole or not at all).
#[derive(Default)]
pub struct Fetch {
    download: Download,
    activity: Activity,
}

impl Fetch {
    /// Starts the download into `dir`.
    ///
    /// # Errors
    /// When it cannot start (already running, no place to fetch from).
    pub fn start(&mut self, dir: &std::path::Path) -> Result<(), String> {
        self.download.start(
            dir,
            lensfun_download::mirrors(),
            lensfun_download::FILES,
            &self.activity,
        )
    }

    /// How it goes.
    #[must_use]
    pub fn progress(&self) -> Progress {
        match self.download.state() {
            State::Idle => Progress::Idle,
            State::Running { bytes, total } => Progress::Running { bytes, total },
            State::Done { .. } => Progress::Done,
            State::Failed(e) => Progress::Failed(e),
            State::Cancelled => Progress::Failed("cancelled".into()),
        }
    }
}

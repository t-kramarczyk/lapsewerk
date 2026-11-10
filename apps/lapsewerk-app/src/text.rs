//! Every text of the app in German and English. A struct, not a lookup table: the compiler checks
//! that both languages have every text.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    De,
    En,
}

impl Lang {
    /// German when the system speaks German, else English.
    #[must_use]
    pub fn from_system() -> Self {
        let wanted = ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.is_empty())
            .or_else(apple_locale)
            .unwrap_or_default();
        // Nothing set at all: German, the app's own language.
        if wanted.is_empty() || wanted.to_ascii_lowercase().starts_with("de") {
            Self::De
        } else {
            Self::En
        }
    }

    #[must_use]
    pub fn t(self) -> &'static Texts {
        match self {
            Self::De => &DE,
            Self::En => &EN,
        }
    }

    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::De => "de",
            Self::En => "en",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "de" => Some(Self::De),
            "en" => Some(Self::En),
            _ => None,
        }
    }
}

/// The macOS user language (apps started from the Finder have no `LANG`).
fn apple_locale() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split('"').nth(1).map(str::to_owned)
}

pub struct Texts {
    pub tagline: &'static str,
    pub new_project: &'static str,
    pub new_project_hint: &'static str,
    pub drop_here: &'static str,
    pub recent: &'static str,
    pub no_recent: &'static str,
    pub remove_from_list: &'static str,
    pub folder_missing: &'static str,
    pub frames: &'static str,
    pub step_word: &'static str,
    pub opened_ago: [&'static str; 5],
    pub projects: &'static str,
    pub steps: [&'static str; 6],
    pub step_hints: [&'static str; 6],
    pub next: &'static str,
    pub back: &'static str,
    pub cancel: &'static str,
    pub this_step: &'static str,
    pub time_left: &'static str,
    pub estimating: &'static str,
    pub measure_again: &'static str,
    pub measure_again_all: &'static str,
    pub measure_again_exposure: &'static str,
    pub measure_again_photos: &'static str,
    pub measure_again_photos_some: &'static str,
    pub measure_again_setup: &'static str,
    pub measure_again_none: &'static str,
    /// After [`Self::measure_again`]: which keyframe(s) were edited (`{list}`: "300" or "1, 300").
    pub measure_again_which: &'static str,
    pub working: [&'static str; 8],
    /// The check pass's round (`{k}` of 2).
    pub pass_of: &'static str,
    /// In the middle while measuring or correcting: why it runs, and that it runs once.
    pub only_once_why: &'static str,
    pub calibrate: &'static str,
    pub calibrate_tip: &'static str,
    pub calibrate_hint: &'static str,
    pub about: &'static str,
    pub calibrate_done: &'static str,
    pub calibrate_none: &'static str,
    pub intro: [(&'static str, &'static str); 4],
    pub intro_next: &'static str,
    pub intro_back: &'static str,
    pub intro_go: &'static str,
    pub intro_skip: &'static str,
    pub intro_again: &'static str,
    pub help: &'static str,
    pub help_tip: &'static str,
    pub help_steps: [&'static str; 6],
    pub help_shooting_title: &'static str,
    pub help_shooting: &'static str,
    pub tween: &'static str,
    pub tween_detail: &'static str,
    pub tween_count: &'static str,
    pub tween_longer: &'static str,
    pub jump_to_frame: &'static str,
    pub clipped_share: &'static str,
    pub length_word: &'static str,
    pub with_gap_frames: &'static str,
    /// As `with_gap_frames`, for one frame.
    pub with_gap_frame: &'static str,
    pub short_video_tip: &'static str,
    pub size_word: &'static str,
    pub as_preset: &'static str,
    pub ntsc: &'static str,
    pub transfer_word: &'static str,
    pub rec709: &'static str,
    pub rec709_tip: &'static str,
    pub auto: &'static str,
    pub auto_tip: &'static str,
    pub wb_auto: &'static str,
    pub wb_pick: &'static str,
    pub wb_pick_tip: &'static str,
    pub frames_there_question: &'static str,
    pub frames_there_count: &'static str,
    pub frames_other_question: &'static str,
    pub replaces_other: &'static str,
    pub resume: &'static str,
    pub replace_frames: &'static str,
    pub export_frames: &'static str,
    pub queue_add: &'static str,
    pub queue_add_replace: &'static str,
    pub queue_add_resume: &'static str,
    pub target_stopped_anew: &'static str,
    pub target_queued: &'static str,
    pub target_foreign: &'static str,
    pub sidecar_unreadable: &'static str,
    pub sidecar_kept: &'static str,
    /// A keyframe (`{frame}`) holding settings (`{names}`) a newer LightCraft wrote.
    pub sidecar_unknown: &'static str,
    /// A keyframe's sidecar saved damaged while the project is open (`{frames}`).
    pub sidecar_broken_live: &'static str,
    pub sidecar_other_editor: &'static str,
    pub project_damaged: &'static str,
    pub project_newer: &'static str,
    pub frames_done_question: &'static str,
    pub target_stopped_unknown: &'static str,
    pub target_done_unknown: &'static str,
    pub target_other: &'static str,
    /// Below [`Self::target_other`]: what differs (`{why}`).
    pub target_why: &'static str,
    pub target_stopped: &'static str,
    pub footer_resume: &'static str,
    /// Beside "Go on" after "Check delivery" found `{n}` frames changed or missing.
    pub footer_damaged: &'static str,
    pub damaged_changed: &'static str,
    pub damaged_missing: &'static str,
    /// The same for a stopped export: going on makes them, then the rest.
    pub footer_damaged_stopped: &'static str,
    pub target_done: &'static str,
    /// Finished there, but "Check delivery" found `{bad}` frames (`{names}`) changed or missing.
    pub target_damaged: &'static str,
    /// A stopped export with frames changed or missing among those there.
    pub target_damaged_stopped: &'static str,
    /// The finished card's title once "Check delivery" found frames changed or missing.
    pub export_done_damaged: &'static str,
    /// Made with other settings, said with what differs below it (no list of guesses).
    pub target_other_short: &'static str,
    pub target_name_clash: &'static str,
    pub name_clash_tip: &'static str,
    pub name_clash_footer: &'static str,
    pub space_free: &'static str,
    /// Lapsewerk's frames there under another name (`{other}`): the folder renamed.
    pub target_renamed: &'static str,
    /// Beside a finished sequence: what is seen without reading the frames, and the check.
    pub target_done_check: &'static str,
    pub queue_add_frames: &'static str,
    pub queue_resume: &'static str,
    pub queue_replace: &'static str,
    pub queue_title: &'static str,
    pub queue_hint: &'static str,
    pub queue_no_cli: &'static str,
    pub queue_waiting: &'static str,
    pub queue_paused: &'static str,
    pub queue_damaged: &'static str,
    pub queue_start: &'static str,
    pub queue_held: &'static str,
    pub queue_remove: &'static str,
    pub queue_clear: &'static str,
    pub queue_finished: &'static str,
    pub queue_stopped: &'static str,
    pub queue_error: &'static str,
    pub queue_done: &'static str,
    pub queue_failed: &'static str,
    pub queue_added: &'static str,
    pub queue_twice: &'static str,
    pub queue_step_start: &'static str,
    pub queue_step_measure: &'static str,
    pub queue_step_check: &'static str,
    pub queue_step_verify: &'static str,
    pub queue_step_dust: &'static str,
    pub queue_step_render: &'static str,
    pub ntsc_only: &'static str,
    pub timecode_tip: &'static str,
    pub timecode_bad: &'static str,
    pub timecode_drop: &'static str,
    pub upright_photos: &'static str,
    pub timecode_drop_skipped: &'static str,
    pub timecode_frame: &'static str,
    pub timecode_empty: &'static str,
    pub sequence_note: &'static str,
    pub play_again: &'static str,
    pub lower_part: &'static str,
    pub flicker_estimate: &'static str,
    pub flicker_estimate_tip: &'static str,
    pub subpixel: &'static str,
    pub subpixel_tip: &'static str,
    pub sequence_word: &'static str,
    pub upscale_warning: &'static str,
    pub upscale_hint: &'static str,
    pub upscale_zoom_hint: &'static str,
    pub upscale_hint_smallest: &'static str,
    pub cli_line: &'static str,
    pub copy_line: &'static str,
    pub sequence_out: &'static str,
    pub sequence_detail: &'static str,
    pub frame_format: &'static str,
    pub tiff_detail: &'static str,
    pub dpx_detail: &'static str,
    pub exr_detail: &'static str,
    pub exr_detail_sdr: &'static str,
    pub fps_short: &'static str,
    pub light_even: &'static str,
    pub light_real: &'static str,
    pub cancelled: &'static str,
    pub error: &'static str,
    // Step 1
    pub hints: &'static str,
    pub no_hints: &'static str,
    pub outliers_found: &'static str,
    pub replace_outliers: &'static str,
    pub clipped_found: &'static str,
    pub clipped_tip: &'static str,
    pub queue_disk_full: &'static str,
    pub gaps_found: &'static str,
    pub fill_gaps: &'static str,
    pub camera: &'static str,
    pub gentle: &'static str,
    pub undo: &'static str,
    pub redo: &'static str,
    pub undo_keys: &'static str,
    pub redo_keys: &'static str,
    pub strong: &'static str,
    pub strength_tip: &'static str,
    pub proof: &'static str,
    pub proof_detail: &'static str,
    pub proof_name: &'static str,
    pub proof_done: &'static str,
    pub camera_detail: &'static str,
    pub camera_moves: [&'static str; 5],
    pub camera_amount: &'static str,
    pub camera_across: &'static str,
    pub camera_down: &'static str,
    pub camera_start: &'static str,
    pub camera_end: &'static str,
    pub replace_this: &'static str,
    pub replaced: &'static str,
    pub sequence: &'static str,
    pub interval: &'static str,
    pub unknown: &'static str,
    pub video_length: &'static str,
    pub gaps: &'static str,
    pub gap_places: [&'static str; 2],
    pub none: &'static str,
    pub raw_jpeg_note: &'static str,
    pub camera_data: &'static str,
    pub start_analysis: &'static str,
    pub choose_other: &'static str,
    // Step 2
    pub flicker: &'static str,
    pub flicker_evened: &'static str,
    pub flicker_levels: [&'static str; 3],
    pub exposure_steps: &'static str,
    pub exposure_steps_detail: &'static str,
    pub exposure_steps_seen: &'static str,
    pub exposure_steps_none: &'static str,
    pub recommended: &'static str,
    pub light_change: &'static str,
    pub light_change_detail: &'static str,
    pub curve_as_shot: &'static str,
    pub shape_light: &'static str,
    pub curve_after: &'static str,
    pub curve_after_unlifted: &'static str,
    pub curve_scene: &'static str,
    pub stops: &'static str,
    pub curve_axis: &'static str,
    pub frame_word: &'static str,
    // Step 3
    pub keyframes: &'static str,
    pub keyframes_hint: &'static str,
    pub suggested: &'static str,
    pub saved: &'static str,
    pub auto_keyframe: &'static str,
    pub auto_locked: &'static str,
    pub make_auto: &'static str,
    pub make_auto_tip: &'static str,
    pub no_exif_hint: &'static str,
    pub create_suggested: &'static str,
    pub add_keyframe_here: &'static str,
    pub masks: &'static str,
    pub masks_hint: &'static str,
    pub mask_linear: &'static str,
    pub mask_radial: &'static str,
    pub mask_sky: &'static str,
    pub mask_luminance: &'static str,
    pub mask_brush: &'static str,
    pub mask_brush_tip: &'static str,
    pub mask_paint_hint: &'static str,
    pub brush_paint: &'static str,
    pub brush_erase: &'static str,
    pub brush_clear: &'static str,
    pub brush_size: &'static str,
    pub mask_linear_tip: &'static str,
    pub mask_radial_tip: &'static str,
    pub mask_sky_tip: &'static str,
    pub mask_luminance_tip: &'static str,
    pub mask_invert: &'static str,
    pub mask_remove: &'static str,
    pub mask_delete: &'static str,
    pub mask_from_lightcraft: &'static str,
    pub mask_drag_linear: &'static str,
    pub mask_drag_radial: &'static str,
    pub mask_width: &'static str,
    pub mask_height: &'static str,
    pub mask_feather: &'static str,
    pub mask_lum_from: &'static str,
    pub mask_lum_to: &'static str,
    pub copy_button: &'static str,
    pub open_lightcraft: &'static str,
    pub open_lightcraft_tip: &'static str,
    pub lightcraft_opened: &'static str,
    pub lightcraft_missing: &'static str,
    pub lightcraft_reloaded: &'static str,
    pub copy_tip: &'static str,
    pub copy_title: &'static str,
    pub copy_what: &'static str,
    pub copy_to: &'static str,
    pub copy_go: &'static str,
    pub copied: &'static str,
    pub no_other_keyframes: &'static str,
    pub reset_hint: &'static str,
    pub save_keyframe: &'static str,
    pub reset: &'static str,
    pub unsaved: &'static str,
    pub no_keyframes: &'static str,
    // Step 4
    pub finish_hint: &'static str,
    pub deflicker: &'static str,
    pub deflicker_detail: &'static str,
    pub thorough: &'static str,
    pub making: &'static str,
    pub day_night: &'static str,
    pub day_night_detail: &'static str,
    pub hold_blacks: &'static str,
    pub shape_title: &'static str,
    pub shape_hint: &'static str,
    pub shape_reset: &'static str,
    pub shape_add: &'static str,
    pub shape_new_at: &'static str,
    pub shape_has_point: &'static str,
    pub shape_remove: &'static str,
    pub shape_with: &'static str,
    pub shape_without: &'static str,
    pub stops_short: &'static str,
    pub suggested_ring: &'static str,
    pub delivery_details: &'static str,
    /// The same for a movie (no INFO.txt beside it).
    pub delivery_movie: &'static str,
    /// The photos' shape in step 1.
    pub shape_upright: &'static str,
    pub shape_wide: &'static str,
    pub suggested_legend: &'static str,
    pub keyframes_taken: &'static str,
    pub keyframes_taken_suggested: &'static str,
    pub hold_blacks_tip: &'static str,
    pub keep_light: &'static str,
    pub stabilize: &'static str,
    pub stabilize_detail: &'static str,
    pub dust: &'static str,
    pub dust_detail: &'static str,
    pub smooth_motion: &'static str,
    pub smooth_motion_detail: &'static str,
    pub denoise: &'static str,
    pub denoise_detail: &'static str,
    pub night_sky: &'static str,
    pub night_sky_detail: &'static str,
    pub light_pollution: &'static str,
    pub reduce_stars: &'static str,
    pub star_trails: &'static str,
    pub trails_endless: &'static str,
    pub advanced: &'static str,
    pub sky_area: &'static str,
    pub sky_found: &'static str,
    pub sky_found_later: &'static str,
    /// The sky moves in the frame (a panning camera): the share above limits the trails.
    pub sky_moving: &'static str,
    /// A timecode with the sRGB curve.
    pub timecode_srgb: &'static str,
    /// After `sky_found` for a sky that stays: the share above does nothing.
    pub sky_slider_off: &'static str,
    pub sky_not_found: &'static str,
    pub reference_area: &'static str,
    pub whole_frame: &'static str,
    pub upper_part: &'static str,
    pub regional: &'static str,
    pub measure_size: &'static str,
    // Step 5
    pub preview_modes: [&'static str; 3],
    pub play: &'static str,
    pub pause: &'static str,
    pub before: &'static str,
    pub after: &'static str,
    pub preview_note: &'static str,
    pub compute_preview: &'static str,
    // Step 6
    pub format: &'static str,
    pub presets: [(&'static str, &'static str); 4],
    /// The upright preset for upright photos: the other shape (name, what for).
    pub preset_landscape: (&'static str, &'static str),
    /// The same formats for a frame sequence (no codec: only size and look count).
    pub presets_frames: [(&'static str, &'static str); 4],
    pub fps: &'static str,
    pub hdr: &'static str,
    pub hdr_detail: &'static str,
    pub hdr_pq: &'static str,
    pub frame_name: &'static str,
    pub frame_name_tip: &'static str,
    pub frame_name_bad: &'static str,
    pub frame_name_refused: &'static str,
    pub hdr_hlg: &'static str,
    pub hdr_pq_detail: &'static str,
    pub hdr_hlg_detail: &'static str,
    pub save_as: &'static str,
    pub change: &'static str,
    pub export: &'static str,
    pub export_done: &'static str,
    pub export_again: &'static str,
    pub frames_with_gaps: &'static str,
    pub gap_filled: [&'static str; 2],
    pub space_short: &'static str,
    pub space_tip_4k: &'static str,
    pub space_anyway: &'static str,
    pub queue_making: &'static str,
    pub target_busy: &'static str,
    pub target_busy_short: &'static str,
    pub verify_button: &'static str,
    /// Step 5: the finished look measured once ("Measure result"), its tip, and what it found.
    pub measure_result: &'static str,
    pub measure_result_tip: &'static str,
    pub measured_line: &'static str,
    pub measured_worst: &'static str,
    pub measured_jump: &'static str,
    pub measured_step_head: &'static str,
    pub go_to_photo: &'static str,
    pub to_step4: &'static str,
    pub measured_spike_head: &'static str,
    pub measured_jump_size: &'static str,
    pub measured_jump_nodefl: &'static str,
    pub measured_worst_nodefl: &'static str,
    pub measured_wave: &'static str,
    pub measured_wave_visible: &'static str,
    pub measured_wave_drawn: &'static str,
    pub measured_wave_nodefl: &'static str,
    pub measured_wave_look: &'static str,
    pub measured_worst_calm: &'static str,
    /// The verdict before the figure, and the photos left out of it.
    pub measured_calm: &'static str,
    pub measured_slight: &'static str,
    pub measured_visible: &'static str,
    pub measured_left_out: &'static str,
    pub verify_tip: &'static str,
    pub verify_ok: &'static str,
    pub verify_bad: &'static str,
    pub verify_bad_other: &'static str,
    /// A frame of the delivery checked: `{name}` changed, missing or an extra picture beside.
    pub verify_changed: &'static str,
    pub verify_cut: &'static str,
    pub and_more: &'static str,
    pub verify_missing: &'static str,
    pub verify_extra: &'static str,
    pub target_busy_measuring: &'static str,
    pub queue_after: &'static str,
    pub queue_after_replace: &'static str,
    pub target_beyond: &'static str,
    pub target_strangers: &'static str,
    /// Who holds a folder: "Prozess {pid} {on}, seit {since}"; {on} is lock_here or lock_on.
    pub lock_who: &'static str,
    pub lock_seen: &'static str,
    pub lock_here: &'static str,
    pub lock_on: &'static str,
    pub space_line: &'static str,
    pub space_tight: &'static str,
    pub space_none: &'static str,
    pub target_missing: &'static str,
    pub close_queue_title: &'static str,
    pub close_queue_text: &'static str,
    pub close_queue_keep: &'static str,
    pub close_queue_quit: &'static str,
    pub no_photos: &'static str,
    pub one_photo: &'static str,
    /// Damaged photos left out (`{names}`), then what becomes of them: `skipped_mid` for those
    /// inside the series, `skipped_edge_one` / `skipped_edge_many` (`{n}`) for those at its ends.
    pub skipped_files: &'static str,
    pub skipped_mid: &'static str,
    pub skipped_edge_one: &'static str,
    pub skipped_edge_many: &'static str,
    /// The damaged photos, in the hints (`{names}`).
    pub skipped_hint: &'static str,
    /// Below "done": the export is without the damaged photos (`{names}`).
    pub export_left_out: &'static str,
    pub coffee_line: &'static str,
    pub coffee_link: &'static str,
    pub export_done_frames: &'static str,
    pub frames_to_video: &'static str,
    pub proof_done_frames: &'static str,
    pub working_frames: &'static str,
    pub show_in_folder: &'static str,
    pub overwrite_question: &'static str,
    pub needs_mac: &'static str,
}

pub static DE: Texts = Texts {
    tagline: "Zeitraffer aus deinen Fotos – in sechs Schritten.",
    new_project: "Neues Projekt",
    new_project_hint: "Ordner mit den Fotos deiner Serie wählen",
    drop_here: "oder einen Ordner hierher ziehen",
    recent: "Zuletzt bearbeitet",
    no_recent: "Noch keine Projekte. Wähle oben einen Ordner mit deiner Fotoserie.",
    remove_from_list: "Aus der Liste entfernen (die Fotos bleiben)",
    folder_missing: "Ordner nicht gefunden",
    frames: "Bilder",
    step_word: "Schritt",
    opened_ago: [
        "gerade eben",
        "vor {n} Min.",
        "vor {n} Std.",
        "gestern",
        "vor {n} Tagen",
    ],
    projects: "Projekte",
    steps: [
        "Bilder",
        "Analyse",
        "Keyframes",
        "Feinschliff",
        "Vorschau",
        "Export",
    ],
    step_hints: [
        "Prüfe, ob die Serie vollständig ist.",
        "Lapsewerk misst Helligkeit, Flackern und die Belichtungssprünge der Kamera.",
        "Lege an wichtigen Stellen (Keyframes) den Look fest, dazwischen wird weich überblendet. Nichts zu ändern? Dann einfach „Weiter“.",
        "Wähle aus, was Lapsewerk zusätzlich verbessern soll.",
        "Sieh dir das Ergebnis an, bevor du exportierst.",
        "Wähle das Format und erstelle das Video (oder die Bilder dafür).",
    ],
    next: "Weiter",
    back: "Zurück",
    cancel: "Abbrechen",
    this_step: "(für diesen Schritt)",
    time_left: "noch",
    estimating: "Restzeit wird berechnet …",
    measure_again: "Keyframes wurden geändert: {count} von {n} Bildern werden nachgemessen (Bild {range}), die übrigen bleiben.",
    measure_again_all: "Keyframes wurden geändert: alle Bilder werden nachgemessen (ein hellerer oder dunklerer Look ändert die Messung jedes Bildes).",
    measure_again_photos_some: "{count} Foto(s) haben sich geändert (Bild {range}): {again} von {n} Bildern werden neu gemessen, die übrigen bleiben.",
    measure_again_photos: "{count} Foto(s) haben sich geändert (Bild {range}): alle Bilder werden neu gemessen.",
    measure_again_setup: "Messeinstellungen oder Lapsewerk-Version geändert: alle Bilder werden neu gemessen.",
    measure_again_exposure: "Keyframes geändert, nur in der Belichtung: die Messungen wandern mit, nichts wird nachgemessen.",
    measure_again_none: "Keyframes wurden geändert, nur Gradationskurve, Kontrast, Tiefen oder Lichter: nichts muss nachgemessen werden.",
    measure_again_which: "Bearbeitet: Keyframe {list}.",
    pass_of: "Durchgang {k} von 2",
    only_once_why: "Nur einmal nötig: das Ergebnis wird neben den Fotos gespeichert. Vorschau und Export brauchen es; danach geht es mit „Weiter“ weiter.",
    working: [
        "Bilder werden geöffnet",
        "Helligkeit wird gemessen",
        "Ergebnis wird nachgemessen",
        "Ausgleich wird berechnet",
        "Sensorstaub wird gesucht",
        "Video wird erstellt",
        "Vorschau wird erstellt",
        "Fotos werden für das Kameraprofil gelesen",
    ],
    calibrate: "Kamera einmessen",
    calibrate_tip: "Ordner mit vielen deiner RAW-Fotos wählen (mit eingebettetem Kamera-JPEG, möglichst verschiedene Motive und Farben). Lapsewerk misst daraus die Farben deiner Kamera ein; Lapsewerk nutzt das Profil dann für jedes Foto dieser Kamera (und LightCraft, wenn du es auch nutzt).",
    calibrate_hint: "Einmal je Kamera, für die meisten nicht nötig: Farben wie aus der Kamera, auch bei Modellen ohne eingebautes Profil.",
    about: "Lapsewerk {version} · © 2026 Tim Kramarczyk · Freie Software unter der GPL 3.0 oder neuer, ohne jede Gewähr; du darfst sie unter deren Bedingungen weitergeben. Enthält LightCraft und weitere Bausteine; ihre Lizenzen (auch der Schriften): THIRD-PARTY-LICENSES.txt.",
    calibrate_done: "Kameraprofil gespeichert: {models}. Es gilt nach einem Neustart von Lapsewerk (und LightCraft).",
    calibrate_none: "Kein Profil: es braucht mindestens {n} RAW-Fotos einer Kamera mit eingebettetem Kamera-JPEG.",
    intro: [
        (
            "Willkommen bei Lapsewerk",
            "Lapsewerk macht aus einer Fotoserie einen ruhigen, flackerfreien Zeitraffer – aus RAW- oder JPEG-Fotos, ohne Zwischenexport. Deine Originale bleiben dabei immer unverändert.",
        ),
        (
            "Sechs Schritte, der Reihe nach",
            "1 Bilder: Ordner wählen.  2 Analyse: Lapsewerk misst Helligkeit und Flackern und warnt vor Blitzen oder Lücken.  3 Keyframes: an wichtigen Stellen den Look einstellen.  4 Feinschliff: Flackern, Tag ↔ Nacht, Kamerafahrt.  5 Vorschau: ansehen.  6 Export: Video erstellen.",
        ),
        (
            "Keyframes – das Herzstück",
            "Ein Keyframe legt fest, wie das Bild an einer Stelle aussieht. Dazwischen blendet Lapsewerk alles weich über: Regler, Masken, sogar die Lage eines Verlaufs. Lapsewerk schlägt Keyframes vor, wo sich das Licht stark ändert – meist reichen sie.",
        ),
        (
            "Hilfe ist immer da",
            "Das Fragezeichen oben (oder F1) erklärt den Schritt, in dem du gerade bist. Fahre mit der Maus über einen Regler, dann siehst du, was er tut. Unten steht immer, was gerade passiert und wie lange es noch dauert.",
        ),
    ],
    intro_next: "Weiter",
    intro_back: "Zurück",
    intro_go: "Los geht's",
    intro_skip: "Überspringen",
    intro_again: "Einführung ansehen",
    help: "?",
    help_tip: "Hilfe zu diesem Schritt (F1)",
    help_steps: [
        "Wähle den Ordner mit den Fotos deiner Serie (oder zieh ihn ins Fenster). Lapsewerk nimmt bei RAW+JPEG automatisch die RAWs. Hier siehst du, wie viele Bilder es sind, das Intervall und ob die Kamera Bilder ausgelassen hat.\n\nWeiter geht es mit „Analyse starten“.",
        "Lapsewerk misst jedes Bild: Wie stark flackert es? Hat die Kamera die Belichtung in Sprüngen nachgeregelt? Wie stark ändert sich das Licht (z. B. Sonnenuntergang)?\n\nDie Kurve zeigt die Helligkeit: grau wie aufgenommen, orange wie im Video, blau das Licht der Szene. Klick in die Kurve, um ein Bild anzusehen.\n\nUnter „Hinweise“ stehen Blitze, Scheinwerfer, ausgebrannte Lichter und Lücken. Blitze werden durch Nachbarbilder ersetzt, Lücken gefüllt – du kannst beides abschalten.",
        "Lege an den vorgeschlagenen Stellen Keyframes an („Vorschläge als Keyframes anlegen“) und stelle sie ein. Zuerst die Grundeinstellungen: Belichtung, Weißabgleich, Kontrast. Die Belichtungssprünge der Kamera gleicht Lapsewerk selbst aus – stelle nur ein, wie es aussehen soll.\n\nMasken wirken auf einen Teil des Bildes: Verlauf für den Himmel, Radial um die Sonne, Pinsel für Gebäude. Mit „Auf andere übertragen …“ kopierst du z. B. Schärfe und Farben auf alle Keyframes.\n\nGespeichert wird mit „Keyframe speichern“ und, was noch offen ist, beim Weitergehen von selbst.",
        "Die Voreinstellungen passen meistens. Bei Sonnenauf- oder -untergang regelt „Tag ↔ Nacht“, wie viel vom echten Lichtwechsel bleibt (15 % wirkt natürlich). „Schwarz bleibt schwarz“ verhindert, dass aufgehellte Nachtbilder flau werden. Mit „Helligkeit von Hand formen“ setzt du Punkte auf die Kurve und machst das Video dort heller oder dunkler – zusätzlich zu Tag ↔ Nacht. Verwacklung bei Wind, Staub auf dem Sensor, Rauschen in der Nacht: jeweils einschalten, wenn nötig.\n\nUnter „Bildausschnitt und Kamerafahrt“ lässt du die Kamera langsam heranzoomen oder schwenken.",
        "Spiel das Video ab, vergleiche vorher/nachher mit dem Schieber oder geh Bild für Bild durch (Pfeiltasten). Ein auffälliges Bild kannst du hier von Hand ersetzen lassen.\n\n„Probevideo“ rechnet schnell ein kleines Video mit allen Effekten – gut, um vor dem langen Export alles zu prüfen.",
        "Wähle das Format: 4K für den Schnitt, 4K oder Full HD zum Ansehen und Teilen, Hochkant für das Handy (als Videodatei am Mac: ProRes für den Schnitt, sonst HEVC). Die Bildrate bestimmt die Länge (25 Bilder = 1 Sekunde).\n\n„Video erstellen“ (bzw. „Bilder erstellen“ bei einer Bildfolge) rendert jedes Bild in voller Qualität. Abbrechen geht jederzeit: ein halbes Video wird gelöscht, schon fertige Bilder einer Bildfolge bleiben, und „Fortsetzen“ macht dort weiter. „Lieferung prüfen“ liest jedes Bild einer Bildfolge und vergleicht es mit seiner Prüfsumme (nach einer Kopie auf ein NAS oder einen Stick); geänderte oder fehlende Bilder macht „Fortsetzen“ neu.\n\n„In die Warteschlange“ rechnet im Hintergrund, während du an diesem oder einem anderen Projekt weiterarbeitest; unten siehst du Fortschritt und Restzeit. Vor dem Start prüft Lapsewerk, ob der Platz reicht.",
    ],
    help_shooting_title: "Tipps für die Aufnahme",
    help_shooting: "An der Kamera: manuell oder Zeitautomatik mit fester ISO, Weißabgleich fest (nicht Auto), RAW. Intervall: 2–5 s für Wolken, 10–30 s für Sonnenuntergang und Sterne. Mindestens 250 Bilder für 10 Sekunden Video. Stativ fest, Bildstabilisator aus, Autofokus aus.",
    tween: "Zwischenbilder",
    tween_detail: "Berechnet zusätzliche Bilder zwischen den Aufnahmen, die der Bewegung folgen: Wolken und Verkehr laufen flüssiger, oder das Video wird langsamer.",
    tween_count: "Zwischen je zwei Bildern:",
    tween_longer: "Das Video wird {n}-mal so lang (oder bei höherer Bildrate gleich lang und flüssiger).",
    jump_to_frame: "Zu diesem Bild springen",
    clipped_share: "bis {p} % der Bildfläche",
    with_gap_frames: "mit {n} eingefügten Bildern für Lücken",
    with_gap_frame: "mit 1 eingefügten Bild für eine Lücke",
    length_word: "Videolänge",
    short_video_tip: "Sehr kurz: für 10 Sekunden Video braucht es bei {fps} Bildern/s etwa {n} Fotos (oder Zwischenbilder im Feinschliff).",
    size_word: "Größe",
    as_preset: "wie oben gewählt",
    ntsc: "NTSC-Variante (23,976 / 29,97 / 59,94)",
    transfer_word: "Kennlinie",
    rec709: "Rec. 709 (Sendemaster, BT.1886)",
    rec709_tip: "Für Schnitt und Sendung: Rec. 709 mit Gamma 2,4, wie es Studiomonitore und NLEs erwarten. Aus: sRGB, wie Browser und Apple-Geräte Fotos zeigen. Bei ProRes und Bildfolgen von selbst an.",
    auto: "Auto",
    auto_tip: "Belichtung, Kontrast, Lichter, Tiefen, Weiß, Schwarz, Dynamik und Sättigung automatisch (wie die Auto-Taste gängiger Raw-Programme).",
    wb_auto: "Weißabgleich: Auto",
    wb_pick: "Weißabgleich: ins Bild klicken",
    wb_pick_tip: "Dann ins Bild klicken, auf etwas, das grau oder weiß sein soll.",
    frames_there_question: "In diesem Ordner liegen schon Bilder (z. B. von einem abgebrochenen Export). Fortsetzen oder neu erstellen?",
    frames_there_count: "In diesem Ordner liegen schon {n} Bilder (z. B. von einem abgebrochenen Export). Fortsetzen oder neu erstellen?",
    replaces_other: "Ersetzt die {n} {old}-Bilder, die dort liegen.",
    frames_other_question: "Dort liegen {n} {old}-Bilder. Durch {new} ersetzen? (Fortsetzen geht nur im selben Format.)",
    resume: "Fortsetzen",
    replace_frames: "Neu erstellen",
    export_frames: "Bilder erstellen",
    queue_add: "In die Warteschlange",
    queue_add_replace: "In die Warteschlange (ersetzt das Video dort)",
    queue_add_resume: "In die Warteschlange (macht mit den Bildern dort weiter)",
    target_stopped_anew: "Dort liegt ein abgebrochener Export: {n} Bilder. Mit Zwischenbildern, Entrauschen, Bewegungsunschärfe oder Sternbearbeitung geht kein Fortsetzen (sie brauchen die Nachbarbilder): er wird neu erstellt.",
    target_queued: "Wird gerade in der Warteschlange erstellt (Fortschritt unten in der Fußleiste). Bis dahin lässt sich hierhin nichts weiter exportieren.",
    target_foreign: "Dort liegen {n} Dateien, die wie Bilder heißen, aber nicht von Lapsewerk dort erstellt wurden: sie bleiben, wie sie sind, der Export geht dort nicht. Einen leeren Ordner wählen.",
    sidecar_other_editor: "Bild {frames} wurde auch in einem anderen Raw-Programm gespeichert. Lapsewerk entwickelt es aber mit seinen eigenen Einstellungen in der XMP-Datei, die Änderung dort wirkt also nicht. Den Keyframe hier oder in LightCraft einstellen – er gilt als bearbeitet und wird nie überschrieben.",
    sidecar_broken_live: "Die XMP-Datei von Bild {frames} wurde gerade beschädigt gespeichert: Lapsewerk rechnet weiter mit dem Stand davor. In LightCraft erneut speichern behebt es.",
    sidecar_unknown: "Bild {frame} enthält Einstellungen, die diese Lapsewerk-Version nicht kennt ({names}, wohl aus einer neueren LightCraft-Version): sie fehlen im Video, und Speichern dieses Keyframes in Lapsewerk entfernt sie aus der XMP. Ein Update von Lapsewerk nimmt sie auf.",
    sidecar_kept: "Keyframe(s) {frames}: die XMP-Datei ist schreibgeschützt oder verknüpft. Sie behalten ihren Look und folgen den bearbeiteten nicht (Datei beschreibbar machen, damit sie folgen).",
    sidecar_unreadable: "Die XMP-Datei von Bild {frames} ist beschädigt oder von einem Programm, dessen Einstellungen Lapsewerk nicht kennt: das Bild wird mit Standardwerten entwickelt. Neu einstellen und speichern ersetzt sie.",
    project_damaged: "Die Projektdatei (lapsewerk.json) war beschädigt: sie liegt als lapsewerk.json.damaged daneben, die Einstellungen beginnen neu. Keyframes und Fotos sind unverändert.",
    project_newer: "Die Projektdatei (lapsewerk.json) stammt von einer neueren Lapsewerk-Version oder ist nicht lesbar: sie bleibt, wie sie ist, und Einstellungen werden nicht hineingeschrieben.",
    frames_done_question: "Diese Bildfolge ist dort schon fertig, mit denselben Einstellungen. Trotzdem neu erstellen?",
    target_stopped_unknown: "Dort liegt ein abgebrochener Export mit {n} Bildern.",
    target_done_unknown: "Dort liegt eine fertige Bildfolge mit {n} Bildern.",
    target_why: "Anders: {why}.",
    target_other: "Dort liegen {n} Bilder mit anderen Einstellungen (Größe, Bildrate, Kurve, Look oder Ausgleich): sie werden ersetzt.",
    footer_resume: "Abgebrochener Export mit {n} fertigen Bildern: „Fortsetzen“ rechnet nur den Rest.",
    footer_damaged_stopped: "Abgebrochener Export, davon {bad}: „Fortsetzen“ macht sie neu und rechnet dann den Rest.",
    damaged_changed: "{n} Bild(er) passen nicht mehr zu ihrer Prüfsumme",
    damaged_missing: "{n} Bild(er) fehlen",
    footer_damaged: "{bad}: „Fortsetzen“ macht nur sie neu.",
    target_stopped: "Dort liegt ein abgebrochener Export: {n} Bilder, mit denselben Einstellungen. „Fortsetzen“ rechnet nur den Rest.",
    target_done: "Dort liegt diese Bildfolge schon fertig: {n} Bilder, mit denselben Einstellungen.",
    target_renamed: "Hier liegen Lapsewerk-Bilder namens {other}_… (Ordner umbenannt?): als Bildname „{other}“ eintragen (oder den Ordner wieder so nennen), um fortzusetzen; zum Ersetzen diese Bilder erst hinausschieben; oder einen anderen Ordner wählen.",
    export_done_damaged: "Gespeichert, aber nicht mehr ganz wie geschrieben",
    target_damaged_stopped: "Dort liegt ein abgebrochener Export dieser Bildfolge; davon {bad} ({names}): „Fortsetzen“ macht sie neu und rechnet dann den Rest.",
    target_other_short: "Dort liegen {n} Bilder mit anderen Einstellungen: sie werden ersetzt.",
    name_clash_tip: "Im Ordner liegen Bilder mit einem anderen Namen (siehe oben).",
    name_clash_footer: "Im Ordner liegen Bilder namens „{there}“, nicht „{here}“: unter „Erweitert“ den Namen angleichen oder einen anderen Ordner wählen.",
    space_free: "Frei: {free}",
    target_name_clash: "Dort liegen schon Bilder namens „{there}“: Bilder namens „{here}“ kommen nicht dazu, und nichts wird ersetzt. Feld „Name der Bilder“ leeren oder „{there}“ eintragen, oder für „{here}“ einen anderen Ordner wählen.",
    target_damaged: "Dort liegt diese Bildfolge, aber {bad} ({names}): „Fortsetzen“ macht nur sie neu.",
    target_done_check: "Gesehen wird ohne Lesen nur, ob alle Bilder da und vollständig sind. Ob jedes Bild noch genau so ist (nach einer Kopie, auf einem alten Laufwerk), liest „Lieferung prüfen“.",
    queue_add_frames: "In die Warteschlange (ersetzt die {n} {fmt}-Bilder dort)",
    queue_resume: "Fortsetzen",
    queue_replace: "Neu rendern (ersetzt)",
    queue_title: "Warteschlange",
    queue_hint: "Exporte laufen im Hintergrund nacheinander, mit den Einstellungen von jetzt; du kannst weiterarbeiten, auch an anderen Projekten. Beim Schließen der App fragt Lapsewerk nach, solange noch gerechnet wird.",
    queue_no_cli: "Die Warteschlange braucht das Programm „lapsewerk“ (Kommandozeile) neben der App.",
    queue_waiting: "wartet",
    queue_paused: "vom letzten Mal, noch nicht gestartet",
    queue_damaged: "Die Warteschlange vom letzten Mal war beschädigt; was sich nicht lesen ließ, fehlt. Die Datei liegt unter {file}.",
    queue_start: "Starten",
    queue_held: "wartet auf einen anderen Export in diesem Ordner (prüft jede Minute, setzt danach dessen Bilder fort)",
    queue_remove: "Entfernen",
    queue_clear: "Fertige entfernen",
    queue_finished: "fertig",
    queue_stopped: "unterbrochen",
    queue_error: "fehlgeschlagen",
    queue_done: "Export fertig: {name}",
    queue_failed: "Export fehlgeschlagen: {name}",
    queue_added: "In der Warteschlange: {name}",
    queue_twice: "Dieser Export wartet schon in der Warteschlange.",
    queue_step_start: "Start",
    queue_step_measure: "Messen",
    queue_step_check: "Prüfen",
    queue_step_verify: "Nachmessen",
    queue_step_dust: "Staub suchen",
    queue_step_render: "Bilder werden erstellt",
    ntsc_only: "Nur bei 24, 30 und 60 Bildern/s (die NTSC-Raten 23,976, 29,97 und 59,94).",
    timecode_tip: "Timecode des ersten Bilds (HH:MM:SS:FF; bei 29,97/59,94 auch Drop-Frame HH:MM:SS;FF), z. B. 01:00:00:00 für ein Sendeband. Leer: keiner. Steht im Film bzw. im Kopf jeder DPX-/EXR-Datei.",
    timecode_bad: "Kein Timecode bei dieser Bildrate (HH:MM:SS:FF)",
    timecode_frame: "Die letzte Zahl (das Bild) muss kleiner als {n} sein: bei {n} Bildern/s zählt sie 0 bis {n}−1",
    timecode_drop_skipped: "Diesen Drop-Frame-Timecode gibt es nicht: ;00 und ;01 (bei 59,94 ;00 bis ;03) fehlen am Anfang jeder Minute, außer jeder zehnten",
    timecode_drop: "Drop-Frame („;“) gibt es nur bei 29,97 und 59,94 Bildern/s",
    upright_photos: "Deine Fotos sind hochkant, das Video wird es auch (die Maße oben sind schon gedreht).",
    timecode_empty: "leer = ohne Timecode",
    sequence_note: "Ausgabe als Bildfolge – ein Ordner mit Bildern (unter „Erweitert“ umstellbar): Größe und Bildrate des gewählten Formats gelten; als Videodatei nur am Mac.",
    play_again: "Abspielen",
    lower_part: "unterem Teil",
    flicker_estimate: "Flackern: {a} in der Aufnahme → {b} im Video (geschätzt)",
    flicker_estimate_tip: "Blenden von Bild zu Bild, aus der Analyse geschätzt (unter 0,01 sieht man nichts). „Gründlich“ misst am fertigen Bild nach und regelt den Rest nach.",
    subpixel: "Feiner (Bruchteile eines Pixels)",
    subpixel_tip: "Noch ruhiger, das Bild wird dabei minimal weicher. Für sehr ruhige Motive in 4K.",
    sequence_word: "Ordner mit {fmt}-Bildern",
    upscale_warning: "Deine Fotos ({size}) sind kleiner als dieses Video: sie werden {f}-fach vergrößert und wirken weicher. {hint}",
    upscale_hint: "Eine kleinere Größe wählen.",
    upscale_zoom_hint: " Weniger Zoom in der Kamerafahrt (Schritt 4) hilft auch.",
    upscale_hint_smallest: "Full HD ist schon die kleinste Größe – das Video wird trotzdem gut, nur nicht ganz so scharf.",
    cli_line: "Genau so von der Kommandozeile rendern (z. B. über Nacht):",
    copy_line: "Kopieren",
    sequence_out: "Als Bildfolge (TIFF, DPX oder EXR)",
    sequence_detail: "Ein Ordner mit einem Bild je Videobild statt einer Videodatei: für DaVinci Resolve, Premiere, After Effects, und auf jedem System.",
    frame_format: "Dateiformat",
    tiff_detail: "16 Bit, mit Farbprofil: passt überall.",
    dpx_detail: "10 Bit, mit der Kennlinie von oben (Rec. 709 oder sRGB, bei HDR PQ bzw. HLG): Standard in Kino und Fernsehen.",
    exr_detail: "Lineares Licht als Gleitkomma, mit HDR auch die Lichter über Weiß: für das Grading (Resolve, Nuke). Große Dateien.",
    exr_detail_sdr: "Lineares Display-Licht als Gleitkomma, 0 bis 1 (über Weiß nur mit HDR-Video): für das Grading (Resolve, Nuke). Große Dateien.",
    fps_short: "Bilder/s",
    light_even: "gleich hell",
    light_real: "so dunkel wie echt",
    cancelled: "Abgebrochen.",
    error: "Das hat nicht geklappt",
    hints: "Hinweise",
    no_hints: "Keine Blitze, Lücken oder ausgebrannten Lichter gefunden.",
    outliers_found: "Auffällige Einzelbilder (Blitz, Scheinwerfer …): {n}",
    replace_outliers: "durch Nachbarbilder ersetzen",
    clipped_found: "Bilder mit ausgebrannten Lichtern: {n}",
    clipped_tip: "Tipp: In den Keyframes „Lichter“ oder „Weiß“ senken.",
    queue_disk_full: "Datenträger voll: Platz schaffen, dann „Fortsetzen“.",
    gaps_found: "Lücken in der Aufnahme (fehlende oder beschädigte Fotos): {n}",
    fill_gaps: "mit Zwischenbildern füllen",
    camera: "Bildausschnitt und Kamerafahrt",
    gentle: "sanft",
    undo: "Rückgängig",
    redo: "Wiederholen",
    undo_keys: "Cmd+Z / Strg+Z",
    redo_keys: "Umschalt+Cmd+Z / Strg+Y",
    strong: "kräftig",
    strength_tip: "Kräftig glättet auch langsameres Flackern, folgt aber schnellen echten Lichtwechseln (Wolkenschatten) weniger.",
    proof: "Probevideo",
    proof_detail: "Ein kleines Video (960 × 540) mit allen Einstellungen, schnell gerechnet und gleich abgespielt.",
    proof_name: "Probe",
    proof_done: "Probevideo fertig.",
    camera_detail: "Welcher Teil des Bildes ins Video kommt, und ob die Kamera langsam heranfährt oder schwenkt.",
    camera_moves: [
        "Fest",
        "Heranzoomen",
        "Wegzoomen",
        "Schwenk nach rechts",
        "Schwenk nach links",
    ],
    camera_amount: "Zoom",
    camera_across: "links ↔ rechts",
    camera_down: "oben ↔ unten",
    camera_start: "Anfang",
    camera_end: "Ende",
    replace_this: "Dieses Bild ersetzen",
    replaced: "wird ersetzt",
    sequence: "Deine Serie",
    interval: "Intervall",
    unknown: "unbekannt",
    video_length: "Videolänge",
    gaps: "Lücken",
    gap_places: ["Stelle", "Stellen"],
    none: "keine",
    raw_jpeg_note: "{n} JPEGs neben ihren Raw-Dateien werden nicht verwendet – das Video entsteht aus den Raws.",
    camera_data: "Mit Belichtungsangaben (EXIF)",
    start_analysis: "Analyse starten",
    choose_other: "Anderen Ordner wählen",
    flicker: "Flackern",
    flicker_levels: ["kaum", "deutlich", "stark"],
    flicker_evened: "{n} Blenden · wird in Schritt 4 ausgeglichen",
    exposure_steps: "Belichtungssprünge der Kamera",
    exposure_steps_detail: "{n} gefunden, alle werden ausgeglichen",
    exposure_steps_seen: "ohne Kameradaten in den Bildern erkannt (bei Bild {frames}) und ausgeglichen",
    exposure_steps_none: "keine gefunden",
    recommended: "empfohlen",
    light_change: "Lichtwechsel",
    light_change_detail: "{n} Blenden zwischen hellstem und dunkelstem Moment",
    curve_as_shot: "wie aufgenommen",
    shape_light: "Licht (ungeglättet)",
    curve_after: "im Video",
    curve_after_unlifted: "im Video (noch ohne Auto-Helligkeit)",
    curve_scene: "Licht der Szene",
    stops: "Blenden",
    curve_axis: "Helligkeit (Blenden)",
    frame_word: "Bild",
    keyframes: "Keyframes",
    keyframes_hint: "Wähle ein Bild, stelle es ein und speichere. Lapsewerk schlägt Stellen vor, an denen sich das Licht stark ändert. Nicht bearbeitete Vorschläge zwischen bearbeiteten passen sich ihnen an; die übrigen behalten ihren Look.",
    suggested: "Vorschlag",
    saved: "gespeichert",
    auto_keyframe: "automatisch",
    auto_locked: "schreibgeschützt",
    make_auto: "Wieder automatisch",
    make_auto_tip: "Dieser Keyframe folgt wieder den bearbeiteten (Auto-Übergang): seine Einstellungen werden aus den Nachbarn überblendet und gespeichert. Bearbeitest du ihn danach (hier, in LightCraft oder einem anderen Raw-Programm), gilt er wieder als bearbeitet.",
    no_exif_hint: "Die Fotos haben keine Belichtungsangaben (EXIF). Der Helligkeitsverlauf kann dadurch flacher ausfallen, als er war: in Schritt 4 lässt er sich von Hand formen.",
    create_suggested: "Vorschläge als Keyframes anlegen",
    add_keyframe_here: "Keyframe bei Bild",
    masks: "Masken",
    masks_hint: "Wirken nur auf einen Teil des Bildes und blenden zwischen den Keyframes über. Masken werden nach ihrer Reihenfolge zugeordnet.",
    mask_linear: "Verlauf",
    mask_radial: "Radial",
    mask_sky: "Himmel",
    mask_luminance: "Helligkeit",
    mask_brush: "Pinsel",
    mask_brush_tip: "Von Hand malen: genau dort, wo du willst, zum Beispiel über einem Gebäude oder Baum.",
    mask_paint_hint: "Im Bild malen. Jeder Strich lässt sich rückgängig machen.",
    brush_paint: "Malen",
    brush_erase: "Radieren",
    brush_clear: "Alles löschen",
    brush_size: "Größe",
    mask_linear_tip: "Ein Verlauf, zum Beispiel von oben bis zum Horizont: volle Wirkung am Anfang, keine am Ende.",
    mask_radial_tip: "Eine Ellipse, zum Beispiel um Sonne, Mond oder ein Gebäude.",
    mask_sky_tip: "Der Himmel, in jedem Bild neu erkannt: folgt Wolken, Bäumen und dem Horizont.",
    mask_luminance_tip: "Nur die hellen (oder dunklen) Teile, zum Beispiel ein heller Himmel ohne den Vordergrund.",
    mask_invert: "Umkehren",
    mask_remove: "Maske löschen",
    mask_delete: "Löschen",
    mask_from_lightcraft: "aus LightCraft",
    mask_drag_linear: "Anfang und Ende im Bild ziehen.",
    mask_drag_radial: "Mitte und Ränder im Bild ziehen.",
    mask_width: "Breite",
    mask_height: "Höhe",
    mask_feather: "Weiche Kante",
    mask_lum_from: "Helligkeit von",
    mask_lum_to: "Helligkeit bis",
    copy_button: "Auf andere übertragen …",
    open_lightcraft: "In LightCraft öffnen",
    open_lightcraft_tip: "Für alles, was hier fehlt (Kurvenpunkte, Objektivprofile): der Keyframe öffnet sich in LightCraft. Dort mit „Metadaten in Datei speichern“ sichern (oder automatisches XMP-Schreiben einschalten), dann übernimmt Lapsewerk die Änderungen von selbst.",
    lightcraft_opened: "In LightCraft geöffnet. Speichere dort die Metadaten in die Datei, dann übernimmt Lapsewerk die Änderungen.",
    lightcraft_missing: "LightCraft ließ sich nicht starten (LightCraft installieren oder LAPSEWERK_LIGHTCRAFT setzen)",
    lightcraft_reloaded: "Änderungen von außen (LightCraft) an Bild(ern) {n} übernommen.",
    copy_tip: "Einstellungen dieses Keyframes auf andere übertragen, zum Beispiel Schärfe und Farben.",
    copy_title: "Übertragen von Bild",
    copy_what: "Was",
    copy_to: "Auf",
    copy_go: "Übertragen und speichern",
    copied: "Auf {n} Keyframes übertragen.",
    no_other_keyframes: "Noch keine anderen Keyframes gespeichert.",
    reset_hint: "Doppelklick: zurück auf den Standardwert",
    save_keyframe: "Keyframe speichern",
    reset: "Zurücksetzen",
    unsaved: "Nicht gespeicherte Änderungen",
    no_keyframes: "Noch keine Keyframes. Lege die Vorschläge an oder wähle ein Bild in der Kurve.",
    finish_hint: "Die Voreinstellungen passen für die meisten Serien. Schalte nur ein, was du brauchst.",
    deflicker: "Flackern entfernen",
    deflicker_detail: "Gleicht Helligkeitsschwankungen von Bild zu Bild aus.",
    thorough: "Gründlich: das fertige Bild nachmessen und den Rest ausgleichen (genauer, dauert länger)",
    day_night: "Tag ↔ Nacht",
    day_night_detail: "Wie viel vom echten Lichtwechsel im Video bleiben soll.",
    hold_blacks: "Schwarz bleibt schwarz",
    shape_title: "Helligkeit von Hand formen",
    shape_hint: "Orange: so hell wird das Video. In die Kurve klicken setzt einen Punkt, Ziehen macht dort heller oder dunkler, Doppelklick entfernt ihn. Dazwischen geht es weich über – z. B. die Nacht etwas heller oder die blaue Stunde länger. Die Zahl am Punkt ist die Änderung dort in Blenden; sie kommt zu Tag ↔ Nacht hinzu.",
    shape_reset: "Zurücksetzen",
    shape_add: "+ Punkt",
    shape_new_at: "Neuer Punkt bei Bild",
    shape_has_point: "hat schon einen Punkt",
    shape_remove: "Punkt entfernen",
    shape_with: "mit deinen Punkten",
    shape_without: "ohne",
    stops_short: "Blenden",
    suggested_ring: "Kreis oben: hier einen Keyframe setzen (Vorschlag)",
    delivery_details: "Technische Angaben für den Videoschnitt (INFO.txt)",
    delivery_movie: "Lieferangaben für den Videoschnitt",
    shape_upright: "hochkant",
    shape_wide: "quer",
    suggested_legend: "Vorschlag für Keyframe",
    keyframes_taken: "{n} bearbeitete Keyframe(s) aus deinen XMP-Dateien übernommen (LightCraft oder ein anderes Raw-Programm).",
    keyframes_taken_suggested: "{n} bearbeitete Keyframe(s) aus deinen XMP-Dateien übernommen (LightCraft oder ein anderes Raw-Programm). Die Vorschläge sind freiwillig: das Video geht auch ohne sie.",
    hold_blacks_tip: "Ein aufgehelltes Nachtbild hebt sonst auch das Schwarz an und wirkt flau. So bleiben die dunkelsten Stellen dunkel, Mitten und Lichter werden trotzdem heller.",
    keep_light: "Lichtwechsel behalten",
    stabilize: "Verwacklung ausgleichen",
    stabilize_detail: "Für Aufnahmen bei Wind oder auf weichem Boden.",
    dust: "Sensorstaub entfernen",
    dust_detail: "Findet dunkle Flecken, die in jedem Bild an derselben Stelle sind.",
    smooth_motion: "Bewegung glätten",
    smooth_motion_detail: "Mischt Nachbarbilder – Wolken und Autos wirken weicher.",
    denoise: "Rauschen entfernen",
    denoise_detail: "Mittelt ruhige Bildteile über mehrere Bilder. Gut für Nacht.",
    night_sky: "Nachthimmel",
    night_sky_detail: "Für Sterne und Milchstraße.",
    light_pollution: "Lichtverschmutzung entfernen",
    reduce_stars: "Sterne abdunkeln",
    star_trails: "Sternspuren",
    trails_endless: "endlos",
    advanced: "Erweitert",
    sky_area: "Himmel (Anteil oben)",
    sky_found_later: "(Gefunden in Bild {n}, im ersten Bild nicht: bei einer Kamerafahrt prüfen.)",
    sky_found: "Himmel erkannt ({p} % des Bildes): Sternspuren, Sterne und Lichtverschmutzung wirken bis zum Horizont.",
    sky_slider_off: "Der Regler „Himmel (Anteil oben)“ gilt dann nicht.",
    timecode_srgb: "Mit Timecode geht ein Video meist in Schnitt oder Sendung: dort wird Rec. 709 erwartet. Oben „Rec. 709“ anhaken.",
    sky_moving: "Die Kamera schwenkt: der Himmel liegt in späteren Bildern woanders. Sternspuren entstehen deshalb nur im oberen Anteil (Regler unten), damit Lichter im Vordergrund keine Spuren ziehen – den Anteil über den höchsten Punkt des Vordergrunds legen.",
    sky_not_found: "Kein Himmel erkannt, auch nicht in späteren Bildern (etwa weil das Land so hell und glatt ist wie der Himmel): es gilt der Anteil oben, der hier eingestellt ist.",
    reference_area: "Flackern messen in",
    whole_frame: "ganzem Bild",
    upper_part: "oberem Teil",
    regional: "Regionaler Deflicker (experimentell)",
    measure_size: "Messgröße",
    preview_modes: ["Abspielen", "Vorher / Nachher", "Bild für Bild"],
    play: "Abspielen",
    pause: "Pause",
    before: "Vorher",
    after: "Nachher",
    preview_note: "Die Vorschau zeigt Belichtung, Flackern und Tag ↔ Nacht. Staub, Rauschen und Sterne kommen beim Export dazu.",
    compute_preview: "Vorschau berechnen",
    format: "Format",
    presets: [
        ("4K ProRes", "Für den Videoschnitt (große Datei)"),
        ("4K HEVC", "Beste Wahl zum Ansehen und Teilen"),
        ("Full HD HEVC", "Klein, für Web und Mail"),
        ("Hochkant 4K", "Instagram, TikTok, Handy"),
    ],
    preset_landscape: ("Quer 4K", "Querformat: oben und unten wird abgeschnitten"),
    presets_frames: [
        ("4K für den Schnitt", "Kennlinie für Videoschnitt (Rec. 709)"),
        ("4K", "Für Bildschirm und Web"),
        ("Full HD", "Kleinere Bilder, schneller fertig"),
        ("Hochkant 4K", "Instagram, TikTok, Handy"),
    ],
    fps: "Bilder pro Sekunde",
    hdr: "HDR-Video",
    hdr_detail: "Hellere Lichter auf HDR-Bildschirmen. Nur sinnvoll mit HDR-Bildschirm und HDR-fähigem Schnittprogramm; für Handy, Instagram und normale Bildschirme aus lassen (dort wirken HDR-Bilder blass).",
    hdr_pq: "PQ (HDR10)",
    frame_name: "Name der Bilder",
    frame_name_tip: "Wie die Bilder heißen (z. B. SHOT010_v001 → SHOT010_v001_000001.tif). Leer: wie die Bilder im Ordner schon heißen, sonst wie der Ordner.",
    frame_name_bad: "nur A–Z, Ziffern, - _ . (nicht am Anfang)",
    frame_name_refused: "Der Name der Bilder geht so nicht (nur A–Z, Ziffern, - _ ., nicht mit - oder . am Anfang): unter „Erweitert“ ändern oder leeren.",
    hdr_hlg: "HLG",
    hdr_pq_detail: "Für YouTube, Apple TV und Streaming: Lichter bis 8-mal heller als normales Weiß.",
    hdr_hlg_detail: "Für Fernsehen und Rundfunk; sieht auch auf SDR-Bildschirmen richtig aus (bis 1000 cd/m²).",
    save_as: "Speichern als",
    change: "Ändern …",
    export: "Video erstellen",
    export_done: "Fertig! Dein Video ist gespeichert.",
    export_again: "Erneut erstellen",
    frames_with_gaps: "{n} Fotos + {gaps} = {all}",
    gap_filled: ["Ersatzbild", "Ersatzbilder"],
    space_short: "Vielleicht zu wenig Platz: bis {need} nötig, {free} frei{tip}.",
    space_tip_4k: " (Full HD statt 4K spart drei Viertel)",
    space_anyway: "Trotzdem starten",
    queue_making: "Wird in der Warteschlange erstellt …",
    making: "Wird erstellt …",
    target_busy_short: "Ein anderer Export schreibt gerade hierhin …",
    verify_button: "Lieferung prüfen",
    measure_result: "Ergebnis nachmessen",
    measure_result_tip: "Rechnet den Ausgleich einmal mit und misst das Ergebnis mit dem fertigen Look (Kurven, Kontrast, Tiefen): wie viel Flackern bleibt und wo. Dauert etwa doppelt so lange wie der Ausgleich.",
    measured_line: "Nachgemessen: {v} Blenden Restflackern von Bild zu Bild (sichtbar ab etwa 0,01).",
    measured_calm: "Kein sichtbares Flackern.",
    measured_slight: "Kaum sichtbares Flackern.",
    measured_visible: "Sichtbares Flackern.",
    measured_left_out: "({n} ersetzte oder eingefügte Bilder nicht mitgezählt.)",
    measured_worst_calm: "Unruhigster Abschnitt: Bild {a}–{b} mit {v} Blenden (nicht sichtbar).",
    measured_wave: "Langsame Welle oder Knick um Bild {a}–{b}: bis {v} Blenden (sichtbar ab etwa 0,05).",
    measured_wave_visible: "Sichtbare langsame Welle.",
    measured_wave_drawn: "Dort biegt deine Handkurve, so wie gezeichnet.",
    measured_wave_nodefl: "„Flackern entfernen“ ist aus: in Schritt 4 einschalten?",
    measured_wave_look: "Dort ansehen: liegt da ein Keyframe oder das Ende einer Rampe?",
    measured_jump_nodefl: "„Flackern entfernen“ ist aus: in Schritt 4 einschalten?",
    measured_worst_nodefl: "Unruhigster Abschnitt: Bild {a}–{b} mit {v} Blenden („Flackern entfernen“ ist aus: in Schritt 4 einschalten?).",
    measured_step_head: "Sprung zwischen Foto {a} und {b}: dort ansehen.",
    go_to_photo: "Zu Foto {n}",
    to_step4: "Zu Schritt 4",
    measured_spike_head: "Foto {a} springt heraus: dort ansehen.",
    measured_jump_size: "Sprung: {v} Blenden.",
    measured_jump: "Liegt dort ein Keyframe, eine Stufe der Kamera oder etwas Helles wie der Mond? Dann in Schritt 4 einen Messbereich ohne das Helle wählen.",
    measured_worst: "Unruhigster Abschnitt: Bild {a}–{b} mit {v} Blenden (dort Bild für Bild ansehen: liegt es an einem Keyframe, einer Stufe der Kamera oder etwas Hellem im Bild? Ein Messbereich oder stärkeres Glätten in Schritt 4 kann helfen).",
    verify_tip: "Liest jedes Bild und vergleicht es mit SHA256SUMS.txt (wie „lapsewerk verify“): nach einer Kopie auf einen Stick oder ein NAS.",
    verify_ok: "Geprüft: alle {n} Bilder sind vollständig und unbeschädigt.",
    verify_bad: "{n} Datei(en) weichen ab: {names}. Geänderte oder fehlende Bilder macht „Fortsetzen“ neu; Bilder, die nicht in der Liste stehen, gehören nicht zur Lieferung.",
    verify_bad_other: "{n} Datei(en) weichen ab: {names}. Die Bilder dort sind mit anderen Einstellungen gemacht als jetzt eingestellt: „Bilder erstellen“ macht die ganze Folge neu.",
    verify_changed: "{name}: geändert (passt nicht mehr zu seiner Prüfsumme)",
    verify_cut: "{name}: gekürzt (viel kleiner als die übrigen Bilder)",
    and_more: "… und {n} weitere",
    verify_missing: "{name}: fehlt",
    verify_extra: "{name}: zusätzliches Bild, nicht in der Liste (gehört nicht zur Lieferung)",
    target_busy_measuring: "Ein anderer Export ({who}) hat diesen Ordner belegt und misst noch (noch kein Bild): warten, bis er fertig ist. Liegt er nach einem Absturz oder Neustart nur noch da, die Datei .lapsewerk-lock in diesem Ordner löschen.",
    queue_after: "Danach in die Warteschlange (wartet und setzt dann fort)",
    queue_after_replace: "Danach in die Warteschlange (wartet und ersetzt dann die Bilder)",
    target_beyond: "Hinter dem Ende liegen Bilder, die nicht zu diesem Export gehören: {names}. Ein neuer Export entfernt sie.",
    target_strangers: "Fremde Bilder im Ordner (bleiben liegen, gehören nicht zu dieser Lieferung): {names}",
    lock_who: "Prozess {pid} {on}, gestartet {since}",
    lock_seen: ", zuletzt aktiv {seen}",
    lock_here: "auf diesem Rechner",
    lock_on: "auf {name}",
    target_busy: "Ein anderer Export ({who}) schreibt gerade hierhin ({n} Bilder bisher): warten, bis er fertig ist. Liegt er nach einem Absturz oder Neustart nur noch da, die Datei .lapsewerk-lock in diesem Ordner löschen.",
    space_line: "Platzbedarf höchstens etwa {need} · frei {free}",
    space_tight: "Platz vielleicht knapp: bis zu {need}, frei {free} (eine Schätzung nach oben: gepackte Bilder brauchen oft viel weniger, der Export kann trotzdem starten)",
    space_none: "Kein zusätzlicher Platz nötig (die Bilder dort werden ersetzt oder fortgesetzt) · frei {free}",
    target_missing: "Der Zielordner ist nicht da: {folder}. Ist die Platte angeschlossen? Sonst unter „Ändern …“ einen anderen wählen.",
    close_queue_title: "Die Warteschlange rechnet noch",
    close_queue_text: "Schließt du Lapsewerk jetzt, wird der laufende Export abgebrochen. Beim nächsten Start stehen er und die wartenden wieder in der Warteschlange und starten, wenn du es sagst. Eine Bildfolge lässt sich fortsetzen; ein Video beginnt von vorn.",
    close_queue_keep: "Weiterrechnen lassen",
    close_queue_quit: "Trotzdem schließen",
    no_photos: "In diesem Ordner sind keine Fotos (JPEG, RAW, TIFF oder PNG): „{folder}“. Wähle den Ordner, in dem die Bilder direkt liegen.",
    one_photo: "In diesem Ordner ist nur ein Foto. Für einen Zeitraffer braucht es eine Serie: viele Fotos derselben Szene im Abstand von ein paar Sekunden (für 10 s Video etwa 250).",
    skipped_files: "Beschädigt und weggelassen: {names}.",
    skipped_mid: "In der Serie entstehen an ihrer Stelle Überblendungen aus den Nachbarbildern (Feinschliff → Lücken füllen).",
    skipped_edge_one: "Eins lag am Anfang oder Ende der Serie: das Video wird 1 Bild kürzer.",
    skipped_edge_many: "{n} lagen am Anfang oder Ende der Serie: das Video wird {n} Bilder kürzer.",
    skipped_hint: "Beschädigte Fotos weggelassen: {names}",
    export_left_out: "Ohne die beschädigten Fotos: {names}",
    coffee_line: "Lapsewerk ist kostenlos. Wenn es dir einen Film wert war:",
    coffee_link: "Spendier mir einen Kaffee",
    export_done_frames: "Fertig! Die Bilder deines Videos sind gespeichert.",
    frames_to_video: "Das ist ein Ordner mit einem Bild je Videobild. Ein Video daraus machen: in DaVinci Resolve (kostenlos), Premiere oder Final Cut den Ordner als Bildfolge importieren und als Video ausgeben. Videodateien direkt erstellt Lapsewerk am Mac.",
    proof_done_frames: "Probe fertig (Ordner mit Bildern) – zum Durchblättern; ein abspielbares Probevideo entsteht am Mac.",
    working_frames: "Bilder werden erstellt",
    show_in_folder: "Im Ordner zeigen",
    overwrite_question: "Die Datei gibt es schon. Ersetzen?",
    needs_mac: "Videodateien kann Lapsewerk nur am Mac schreiben. Unter „Erweitert“: „Als Bildfolge“ geht überall.",
};

pub static EN: Texts = Texts {
    tagline: "Time-lapse from your photos – in six steps.",
    new_project: "New project",
    new_project_hint: "Choose the folder with your photo series",
    drop_here: "or drop a folder here",
    recent: "Recent",
    no_recent: "No projects yet. Choose a folder with your photo series above.",
    remove_from_list: "Remove from the list (the photos stay)",
    folder_missing: "Folder not found",
    frames: "frames",
    step_word: "Step",
    opened_ago: [
        "just now",
        "{n} min ago",
        "{n} h ago",
        "yesterday",
        "{n} days ago",
    ],
    projects: "Projects",
    steps: [
        "Photos",
        "Analysis",
        "Keyframes",
        "Finishing",
        "Preview",
        "Export",
    ],
    step_hints: [
        "Check that the series is complete.",
        "Lapsewerk measures brightness, flicker and the camera's exposure steps.",
        "Set the look at key moments (keyframes); Lapsewerk blends smoothly in between. Nothing to change? Just go on.",
        "Choose what else Lapsewerk should improve.",
        "Look at the result before you export.",
        "Choose the format and make the video (or its frames).",
    ],
    next: "Next",
    back: "Back",
    cancel: "Cancel",
    this_step: "(this step)",
    time_left: "",
    estimating: "Working out the time left …",
    measure_again: "Keyframes changed: {count} of {n} frames are measured again (frames {range}), the others kept.",
    measure_again_all: "Keyframes changed: every frame is measured again (a brighter or darker look changes how every frame is measured).",
    measure_again_photos_some: "{count} photo(s) changed (frames {range}): {again} of {n} frames are measured again, the others kept.",
    measure_again_photos: "{count} photo(s) changed (frames {range}): every frame is measured again.",
    measure_again_setup: "Measuring settings or Lapsewerk's version changed: every frame is measured again.",
    measure_again_exposure: "Keyframes changed in exposure only: the measurements move along, nothing is measured again.",
    measure_again_none: "Keyframes changed, only the tone curve, contrast, shadows or highlights: nothing to measure again.",
    measure_again_which: "Edited: keyframe {list}.",
    pass_of: "pass {k} of 2",
    only_once_why: "Needed once: the result is kept beside the photos. The preview and the export need it; then \"Next\" goes on.",
    working: [
        "Opening the photos",
        "Measuring brightness",
        "Measuring the result",
        "Working out the corrections",
        "Looking for sensor dust",
        "Making the video",
        "Making the preview",
        "Reading photos for the camera profile",
    ],
    calibrate: "Calibrate camera",
    calibrate_tip: "Choose folders with many of your raw photos (with the embedded camera JPEG, varied scenes and colours). Lapsewerk measures your camera’s colours from them; Lapsewerk then uses the profile for every photo of that camera (and LightCraft, if you use it too).",
    calibrate_hint: "Once per camera, not needed by most: colours as out of the camera, also for models without a built-in profile.",
    about: "Lapsewerk {version} · © 2026 Tim Kramarczyk · Free software under the GPL 3.0 or later, with no warranty; you may pass it on under its terms. Includes LightCraft and other components; their licences (fonts included): THIRD-PARTY-LICENSES.txt.",
    calibrate_done: "Camera profile saved: {models}. It applies once Lapsewerk (and LightCraft) restart.",
    calibrate_none: "No profile: at least {n} raw photos of one camera with an embedded camera JPEG are needed.",
    intro: [
        (
            "Welcome to Lapsewerk",
            "Lapsewerk turns a series of photos into a calm, flicker-free time-lapse – from raw or JPEG photos, with no export in between. Your originals are never changed.",
        ),
        (
            "Six steps, one after the other",
            "1 Photos: choose the folder.  2 Analysis: Lapsewerk measures brightness and flicker and warns about flashes or gaps.  3 Keyframes: set the look at important points.  4 Finishing: flicker, day ↔ night, camera move.  5 Preview: watch it.  6 Export: make the video.",
        ),
        (
            "Keyframes – the heart of it",
            "A keyframe sets how the picture looks at one point. In between, Lapsewerk blends everything smoothly: sliders, masks, even where a gradient sits. Lapsewerk suggests keyframes where the light changes a lot – usually they are enough.",
        ),
        (
            "Help is always there",
            "The question mark at the top (or F1) explains the step you are in. Hover over a slider to see what it does. The bottom line always says what is happening and how long it will take.",
        ),
    ],
    intro_next: "Next",
    intro_back: "Back",
    intro_go: "Let's go",
    intro_skip: "Skip",
    intro_again: "Show the introduction",
    help: "?",
    help_tip: "Help for this step (F1)",
    help_steps: [
        "Choose the folder with the photos of your series (or drag it into the window). With RAW+JPEG, Lapsewerk takes the raws. Here you see how many photos there are, the interval and whether the camera skipped any.\n\nGo on with “Start analysis”.",
        "Lapsewerk measures every frame: how much does it flicker? Did the camera change exposure in steps? How much does the light change (a sunset, say)?\n\nThe curve shows brightness: grey as shot, orange as in the video, blue the scene’s light. Click into it to see a frame.\n\n“Hints” lists flashes, headlights, burnt-out highlights and gaps. Flashes are replaced by neighbouring frames, gaps filled – you can turn both off.",
        "Create keyframes at the suggested points (“Create the suggested keyframes”) and adjust them. Start with the basics: exposure, white balance, contrast. Lapsewerk evens out the camera’s exposure steps itself – just set how it should look.\n\nMasks act on part of the picture: a gradient for the sky, a radial around the sun, a brush for buildings. “Copy to others …” copies, say, sharpening and colours to every keyframe.\n\n“Save keyframe” saves; what is still open is saved when you move on.",
        "The defaults fit most series. For a sunrise or sunset, “Day ↔ night” sets how much of the real change of light stays (15 % looks natural). “Black stays black” keeps brightened night frames from going flat. With “Shape the brightness by hand” you set points on the curve and make the video brighter or darker there – on top of day ↔ night. Shake in wind, dust on the sensor, noise at night: turn each on when needed.\n\nUnder “Framing and camera move” the camera can slowly zoom in or pan.",
        "Play the video, compare before/after with the slider, or step through frame by frame (arrow keys). An odd frame can be replaced by hand here.\n\n“Test video” quickly makes a small video with every effect – good for checking before the long export.",
        "Choose the format: 4K for editing, 4K or Full HD for watching and sharing, upright for phones (as a movie file on a Mac: ProRes for editing, HEVC otherwise). The frame rate sets the length (25 frames = 1 second).\n\n“Make the video” (“Make the frames” for a frame sequence) renders every frame at full quality. You can cancel at any time: a half-written movie is deleted, the frames of a sequence stay and “Resume” goes on from there. “Check delivery” reads every frame of a sequence and compares it with its checksum (after a copy to a NAS or a stick); “Resume” makes changed or missing frames again.\n\n“Add to queue” renders in the background while you go on with this or another project; progress and time left show at the bottom. Before it starts, Lapsewerk checks there is room.",
    ],
    help_shooting_title: "Tips for shooting",
    help_shooting: "On the camera: manual or aperture priority with fixed ISO, fixed white balance (not auto), raw. Interval: 2–5 s for clouds, 10–30 s for sunsets and stars. At least 250 photos for 10 seconds of video. A steady tripod, stabilisation off, autofocus off.",
    tween: "In-between frames",
    tween_detail: "Computes frames between the shots along the motion (optical flow): clouds and traffic flow more smoothly, or the video gets slower.",
    tween_count: "Between every two frames:",
    tween_longer: "The video gets {n} times longer (or as long and smoother at a higher frame rate).",
    jump_to_frame: "Jump to this frame",
    clipped_share: "up to {p} % of the picture",
    with_gap_frames: "with {n} frames put in for gaps",
    with_gap_frame: "with 1 frame put in for a gap",
    length_word: "Video length",
    short_video_tip: "Very short: 10 seconds of video at {fps} fps take about {n} photos (or in-between frames in Finishing).",
    size_word: "Size",
    as_preset: "as chosen above",
    ntsc: "NTSC variant (23.976 / 29.97 / 59.94)",
    transfer_word: "Transfer",
    rec709: "Rec. 709 (broadcast master, BT.1886)",
    rec709_tip: "For editing and broadcast: Rec. 709 with gamma 2.4, as studio monitors and NLEs expect. Off: sRGB, as browsers and Apple devices show photos. On by itself for ProRes and frame sequences.",
    auto: "Auto",
    auto_tip: "Exposure, contrast, highlights, shadows, whites, blacks, vibrance and saturation automatically (like the Auto button of common raw editors).",
    wb_auto: "White balance: auto",
    wb_pick: "White balance: click in picture",
    wb_pick_tip: "Then click in the picture, on something that should be grey or white.",
    frames_there_question: "This folder already holds frames (a stopped export, say). Go on or make them anew?",
    frames_there_count: "This folder already holds {n} frames (a stopped export, say). Go on or make them anew?",
    replaces_other: "Replaces the {n} {old} frames there.",
    frames_other_question: "{n} {old} frames are there. Replace them with {new}? (Going on works in the same format only.)",
    resume: "Go on",
    replace_frames: "Make anew",
    export_frames: "Make the frames",
    queue_add: "Add to queue",
    queue_add_replace: "Add to queue (replaces the video there)",
    queue_add_resume: "Add to queue (goes on with the frames there)",
    target_stopped_anew: "A stopped export is there: {n} frames. With in-between frames, denoise, motion blur or star work it can't go on (they need the neighbouring frames): it is made anew.",
    target_queued: "Being made in the queue now (progress in the status bar). Nothing else can be exported here until then.",
    target_foreign: "{n} files named like frames that Lapsewerk did not make are there: they stay as they are and the export can't go there. Choose an empty folder.",
    sidecar_other_editor: "Frame {frames} was saved by another raw editor too. Lapsewerk develops it with its own settings in the XMP file, so that edit does not show. Adjust the keyframe here or in LightCraft – it counts as edited and is never written over.",
    sidecar_broken_live: "The XMP file of frame {frames} was just saved damaged: Lapsewerk goes on with the look as before. Saving it again in LightCraft fixes it.",
    sidecar_unknown: "Frame {frame} holds settings this Lapsewerk does not know ({names}, likely from a newer LightCraft): they are missing in the video, and saving this keyframe in Lapsewerk removes them from the XMP. An update of Lapsewerk takes them in.",
    sidecar_kept: "Keyframe(s) {frames}: the XMP file is read-only or linked. They keep their look and don't follow the edited ones (make the file writable to let them).",
    sidecar_unreadable: "The XMP file of frame {frames} is damaged or from a program whose settings Lapsewerk does not know: the frame develops with the defaults. Adjusting and saving it replaces it.",
    project_damaged: "The project file (lapsewerk.json) was damaged: it is kept beside as lapsewerk.json.damaged and the settings start anew. Keyframes and photos are unchanged.",
    project_newer: "The project file (lapsewerk.json) is from a newer Lapsewerk or can't be read: it is left as it is and settings are not written into it.",
    frames_done_question: "This sequence is finished there already, with the same settings. Make it anew all the same?",
    target_stopped_unknown: "A stopped export of {n} frames is there.",
    target_done_unknown: "A finished sequence of {n} frames is there.",
    target_why: "What differs: {why}.",
    target_other: "{n} frames made with other settings (size, rate, curve, look or corrections) are there: they are replaced.",
    footer_resume: "A stopped export with {n} frames done: \"Go on\" renders only the rest.",
    footer_damaged_stopped: "A stopped export; of its frames, {bad}: \"Go on\" makes them again, then renders the rest.",
    damaged_changed: "{n} frame(s) no longer match their checksum",
    damaged_missing: "{n} frame(s) are missing",
    footer_damaged: "{bad}: \"Go on\" makes just them again.",
    target_stopped: "A stopped export is there: {n} frames, with the same settings. \"Go on\" renders only the rest.",
    target_done: "This sequence is there already, finished: {n} frames, with the same settings.",
    target_renamed: "Lapsewerk frames named {other}_… are here (the folder renamed?): enter \"{other}\" as the frames' name (or name the folder so again) to go on; to replace them, move them out first; or choose another folder.",
    export_done_damaged: "Saved, but no longer quite as written",
    target_damaged_stopped: "A stopped export of this sequence is there; of its frames, {bad} ({names}): \"Go on\" makes them again, then renders the rest.",
    target_other_short: "{n} frames made with other settings are there: they are replaced.",
    name_clash_tip: "The folder holds frames of another name (see above).",
    name_clash_footer: "The folder holds frames named \"{there}\", not \"{here}\": match the name under \"Advanced\" or choose another folder.",
    space_free: "Free: {free}",
    target_name_clash: "Frames named \"{there}\" are there already: frames named \"{here}\" are not added beside them, and nothing is replaced. Clear \"Frames' name\" or enter \"{there}\", or choose another folder for \"{here}\".",
    target_damaged: "This sequence is there, but {bad} ({names}): \"Go on\" makes just them again.",
    target_done_check: "Without reading the frames, only whether all are there and whole is seen. Whether each is still exactly as written (after a copy, on an old drive) is read by \"Check delivery\".",
    queue_add_frames: "Add to queue (replaces the {n} {fmt} frames there)",
    queue_resume: "Resume",
    queue_replace: "Render anew (replaces)",
    queue_title: "Queue",
    queue_hint: "Exports run in the background one after another, with the settings as they are now; you can keep working, on other projects too. Closing the app asks first while it is still rendering.",
    queue_no_cli: "The queue needs the command-line program “lapsewerk” beside the app.",
    queue_waiting: "waiting",
    queue_paused: "from last time, not started yet",
    queue_damaged: "The queue from last time was damaged; what could not be read is left out. The file was put aside as {file}.",
    queue_start: "Start",
    queue_held: "waiting for another export in this folder (checks every minute, then goes on with its frames)",
    queue_remove: "Remove",
    queue_clear: "Remove finished",
    queue_finished: "done",
    queue_stopped: "stopped",
    queue_error: "failed",
    queue_done: "Export done: {name}",
    queue_failed: "Export failed: {name}",
    queue_added: "Queued: {name}",
    queue_twice: "This export is already waiting in the queue.",
    queue_step_start: "Starting",
    queue_step_measure: "Measuring",
    queue_step_check: "Checking",
    queue_step_verify: "Verifying",
    queue_step_dust: "Finding dust",
    queue_step_render: "Making the frames",
    ntsc_only: "Only at 24, 30 and 60 fps (the NTSC rates 23.976, 29.97 and 59.94).",
    timecode_tip: "Timecode of the first frame (HH:MM:SS:FF; at 29.97/59.94 also drop-frame HH:MM:SS;FF), e.g. 01:00:00:00 for a broadcast master. Empty: none. Written into the movie or each DPX / EXR frame's header.",
    timecode_bad: "Not a timecode at this frame rate (HH:MM:SS:FF)",
    timecode_frame: "The last number (the frame) must be under {n}: at {n} fps it counts 0 to {n}−1",
    timecode_drop_skipped: "No such drop-frame timecode: ;00 and ;01 (at 59.94 ;00 to ;03) are skipped at each minute's start, but every tenth",
    timecode_drop: "Drop-frame (“;”) exists at 29.97 and 59.94 fps only",
    upright_photos: "Your photos are upright, so the video is too (the sizes above are turned already).",
    timecode_empty: "empty = no timecode",
    sequence_note: "Output as a frame sequence – a folder of pictures (change it under “Advanced”): the size chosen above and rate apply; as a movie file on a Mac only.",
    play_again: "Play",
    lower_part: "the lower part",
    flicker_estimate: "Flicker: {a} as shot → {b} in the video (estimated)",
    flicker_estimate_tip: "Stops frame to frame, estimated from the analysis (below 0.01 nothing shows). “Thorough” measures the finished picture and corrects what is left.",
    subpixel: "Finer (fractions of a pixel)",
    subpixel_tip: "Steadier still, the picture gets a touch softer. For very calm scenes in 4K.",
    sequence_word: "folder of {fmt} frames",
    upscale_warning: "Your photos ({size}) are smaller than this video: they are scaled up {f}× and look softer. {hint}",
    upscale_hint: "Choose a smaller size.",
    upscale_zoom_hint: " Less zoom in the camera move (step 4) helps too.",
    upscale_hint_smallest: "Full HD is already the smallest size – the video still works, just a little softer.",
    cli_line: "Render exactly this from the command line (overnight, say):",
    copy_line: "Copy",
    sequence_out: "As a frame sequence (TIFF, DPX or EXR)",
    sequence_detail: "A folder with one picture per video frame instead of a movie file: for DaVinci Resolve, Premiere, After Effects, and on any system.",
    frame_format: "File format",
    tiff_detail: "16-bit, with a colour profile: works everywhere.",
    dpx_detail: "10-bit, with the curve above (Rec. 709 or sRGB, PQ or HLG for HDR): the film and broadcast standard.",
    exr_detail: "Linear light as floating point, with HDR the highlights above white too: for grading (Resolve, Nuke). Large files.",
    exr_detail_sdr: "Linear display light as floating point, 0 to 1 (above white only with HDR video): for grading (Resolve, Nuke). Large files.",
    fps_short: "fps",
    light_even: "even",
    light_real: "as dark as it was",
    cancelled: "Cancelled.",
    error: "That did not work",
    hints: "Notes",
    no_hints: "No flashes, gaps or burnt-out highlights found.",
    outliers_found: "Odd frames (a flash, headlights …): {n}",
    replace_outliers: "replace by their neighbours",
    clipped_found: "Frames with burnt-out highlights: {n}",
    clipped_tip: "Tip: lower Highlights or Whites in the keyframes.",
    queue_disk_full: "Disk full: make room, then “Resume”.",
    gaps_found: "Gaps in the capture (missing or damaged photos): {n}",
    fill_gaps: "fill with blended frames",
    camera: "Framing and camera move",
    gentle: "gentle",
    undo: "Undo",
    redo: "Redo",
    undo_keys: "Cmd+Z / Ctrl+Z",
    redo_keys: "Shift+Cmd+Z / Ctrl+Y",
    strong: "strong",
    strength_tip: "Strong also smooths slower flicker but follows quick real changes of light (cloud shadows) less.",
    proof: "Test video",
    proof_detail: "A small video (960 × 540) with all settings, made quickly and played right away.",
    proof_name: "test",
    proof_done: "Test video done.",
    camera_detail: "Which part of the frame the video shows, and whether the camera slowly moves in or pans.",
    camera_moves: ["Still", "Zoom in", "Zoom out", "Pan right", "Pan left"],
    camera_amount: "Zoom",
    camera_across: "left ↔ right",
    camera_down: "top ↔ bottom",
    camera_start: "Start",
    camera_end: "End",
    replace_this: "Replace this frame",
    replaced: "is replaced",
    sequence: "Your series",
    interval: "Interval",
    unknown: "unknown",
    video_length: "Video length",
    gaps: "Gaps",
    gap_places: ["place", "places"],
    none: "none",
    raw_jpeg_note: "{n} JPEGs beside their raw files are not used – the video is made from the raws.",
    camera_data: "With exposure data (EXIF)",
    start_analysis: "Start analysis",
    choose_other: "Choose another folder",
    flicker: "Flicker",
    flicker_levels: ["hardly any", "noticeable", "strong"],
    flicker_evened: "{n} stops · evened out in step 4",
    exposure_steps: "Camera exposure steps",
    exposure_steps_detail: "{n} found, all evened out",
    exposure_steps_seen: "seen in the pictures, no camera data (at frame {frames}), evened out",
    exposure_steps_none: "none found",
    recommended: "recommended",
    light_change: "Change of light",
    light_change_detail: "{n} stops between the brightest and the darkest moment",
    curve_as_shot: "as shot",
    shape_light: "light (unsmoothed)",
    curve_after: "in the video",
    curve_after_unlifted: "in the video (auto brightness not yet in)",
    curve_scene: "scene light",
    stops: "stops",
    curve_axis: "Brightness (stops)",
    frame_word: "Frame",
    keyframes: "Keyframes",
    keyframes_hint: "Pick a frame, adjust it and save. Lapsewerk suggests frames where the light changes a lot. Suggestions you do not edit follow the edited ones they lie between; the others keep their look.",
    suggested: "suggested",
    saved: "saved",
    auto_locked: "read-only",
    auto_keyframe: "automatic",
    make_auto: "Automatic again",
    make_auto_tip: "This keyframe follows the edited ones again (auto transition): its settings are blended from its neighbours and saved. Edit it afterwards (here, in LightCraft or another raw editor) and it counts as edited again.",
    no_exif_hint: "The photos carry no exposure data (EXIF). The change of brightness may come out flatter than it was: step 4 lets you shape it by hand.",
    create_suggested: "Create the suggested keyframes",
    add_keyframe_here: "Keyframe at frame",
    masks: "Masks",
    masks_hint: "Act on part of the frame only and blend between keyframes. Masks are matched by their order.",
    mask_linear: "Gradient",
    mask_radial: "Radial",
    mask_sky: "Sky",
    mask_luminance: "Brightness",
    mask_brush: "Brush",
    mask_brush_tip: "Paint by hand: exactly where you want, for example over a building or a tree.",
    mask_paint_hint: "Paint in the picture. Every stroke can be undone.",
    brush_paint: "Paint",
    brush_erase: "Erase",
    brush_clear: "Clear all",
    brush_size: "Size",
    mask_linear_tip: "A gradient, for example from the top to the horizon: full effect at the start, none at the end.",
    mask_radial_tip: "An ellipse, for example around the sun, the moon or a building.",
    mask_sky_tip: "The sky, found anew in every frame: it follows clouds, trees and the horizon.",
    mask_luminance_tip: "Only the bright (or dark) parts, for example a bright sky without the foreground.",
    mask_invert: "Invert",
    mask_remove: "Delete mask",
    mask_delete: "Delete",
    mask_from_lightcraft: "from LightCraft",
    mask_drag_linear: "Drag the start and the end in the picture.",
    mask_drag_radial: "Drag the centre and the edges in the picture.",
    mask_width: "Width",
    mask_height: "Height",
    mask_feather: "Feather",
    mask_lum_from: "Brightness from",
    mask_lum_to: "Brightness to",
    copy_button: "Copy to others …",
    open_lightcraft: "Open in LightCraft",
    open_lightcraft_tip: "For whatever is missing here (curve points, lens profiles): the keyframe opens in LightCraft. Save its metadata to the file there (or turn on automatic XMP writing) and Lapsewerk picks the changes up by itself.",
    lightcraft_opened: "Opened in LightCraft. Save the metadata to the file there and Lapsewerk picks the changes up.",
    lightcraft_missing: "LightCraft could not be started (install LightCraft or set LAPSEWERK_LIGHTCRAFT)",
    lightcraft_reloaded: "Changes made outside (LightCraft) taken over for frame(s) {n}.",
    copy_tip: "Copy settings of this keyframe to others, such as sharpening and colours.",
    copy_title: "Copy from frame",
    copy_what: "What",
    copy_to: "To",
    copy_go: "Copy and save",
    copied: "Copied to {n} keyframes.",
    no_other_keyframes: "No other keyframes saved yet.",
    reset_hint: "Double-click: back to the default",
    save_keyframe: "Save keyframe",
    reset: "Reset",
    unsaved: "Unsaved changes",
    no_keyframes: "No keyframes yet. Create the suggested ones or pick a frame in the curve.",
    finish_hint: "The defaults suit most series. Turn on only what you need.",
    deflicker: "Remove flicker",
    deflicker_detail: "Evens out brightness changes from frame to frame.",
    thorough: "Thorough: measure the finished picture again and even out what is left (more exact, takes longer)",
    day_night: "Day ↔ night",
    day_night_detail: "How much of the real change of light stays in the video.",
    hold_blacks: "Black stays black",
    shape_title: "Shape the brightness by hand",
    shape_hint: "Orange: how bright the video gets. Click on the curve to set a point, drag it to make the video brighter or darker there, double-click to take it away. In between it changes smoothly – a brighter night, say, or a longer blue hour. The figure at a point is the change there, in stops; it comes on top of day ↔ night.",
    shape_reset: "Reset",
    shape_add: "+ Point",
    shape_new_at: "New point at frame",
    shape_has_point: "has a point already",
    shape_remove: "Remove the point",
    shape_with: "with your points",
    shape_without: "without",
    stops_short: "stops",
    suggested_ring: "ring on top: a keyframe would help here (suggestion)",
    delivery_details: "Technical details for editing (INFO.txt)",
    delivery_movie: "Delivery details for editing",
    shape_upright: "upright",
    shape_wide: "landscape",
    suggested_legend: "keyframe suggestion",
    keyframes_taken: "{n} edited keyframe(s) taken from your XMP files (LightCraft or another raw editor).",
    keyframes_taken_suggested: "{n} edited keyframe(s) taken from your XMP files (LightCraft or another raw editor). The suggestions are optional: the video works without them.",
    hold_blacks_tip: "A brightened night frame lifts its blacks too and looks flat. This keeps the darkest parts dark while mid-tones and highlights still get brighter.",
    keep_light: "Keep the change of light",
    stabilize: "Remove camera shake",
    stabilize_detail: "For shots in wind or on soft ground.",
    dust: "Remove sensor dust",
    dust_detail: "Finds dark spots in the same place in every frame.",
    smooth_motion: "Smooth motion",
    smooth_motion_detail: "Blends neighbouring frames – clouds and cars look softer.",
    denoise: "Remove noise",
    denoise_detail: "Averages still parts over several frames. Good for night.",
    night_sky: "Night sky",
    night_sky_detail: "For stars and the Milky Way.",
    light_pollution: "Remove light pollution",
    reduce_stars: "Dim the stars",
    star_trails: "Star trails",
    trails_endless: "endless",
    advanced: "Advanced",
    sky_area: "Sky (upper part)",
    sky_found_later: "(Found in frame {n}, not in the first: check it with a camera move.)",
    sky_found: "Sky found ({p} % of the frame): star trails, stars and light pollution work down to the horizon.",
    sky_slider_off: "The \"Sky (upper part)\" slider does not apply then.",
    timecode_srgb: "With a timecode a video mostly goes to an edit or a broadcast, which expect Rec. 709: tick \"Rec. 709\" above.",
    sky_moving: "The camera pans: the sky lies elsewhere in later frames. Star trails therefore form only in the upper part (slider below), so foreground lights leave none – set it above the foreground's highest point.",
    sky_not_found: "No sky found, in later frames either (the land as bright and smooth as the sky?): the upper part set here is used.",
    reference_area: "Measure flicker in",
    whole_frame: "the whole frame",
    upper_part: "the upper part",
    regional: "Regional deflicker (experimental)",
    measure_size: "Measuring size",
    preview_modes: ["Play", "Before / after", "Frame by frame"],
    play: "Play",
    pause: "Pause",
    before: "Before",
    after: "After",
    preview_note: "The preview shows exposure, flicker and day ↔ night. Dust, noise and stars are added on export.",
    compute_preview: "Make the preview",
    format: "Format",
    presets: [
        ("4K ProRes", "Best quality for editing"),
        ("4K HEVC", "Small file to share"),
        ("Full HD HEVC", "Small, for web and mail"),
        ("Upright 4K", "For phones and social media"),
    ],
    preset_landscape: ("Landscape 4K", "Wide: the top and bottom are cut off"),
    presets_frames: [
        ("4K for editing", "Video editing look (Rec. 709)"),
        ("4K", "For screens and the web"),
        ("Full HD", "Smaller frames, done sooner"),
        ("Upright 4K", "For phones and social media"),
    ],
    fps: "Frames per second",
    hdr: "HDR video",
    hdr_detail: "Brighter highlights on HDR screens. Only with an HDR screen and an HDR-capable editor; leave it off for phones, Instagram and ordinary screens (HDR pictures look pale there).",
    hdr_pq: "PQ (HDR10)",
    frame_name: "Frames' name",
    frame_name_tip: "What the frames are called (e.g. SHOT010_v001 → SHOT010_v001_000001.tif). Empty: as the frames in the folder are named already, else after the folder.",
    frame_name_bad: "only A–Z, digits, - _ . (not first)",
    frame_name_refused: "The frames' name can't be used (only A–Z, digits, - _ ., not starting with - or .): change or clear it under \"Advanced\".",
    hdr_hlg: "HLG",
    hdr_pq_detail: "For YouTube, Apple TV and streaming: highlights up to 8 times brighter than ordinary white.",
    hdr_hlg_detail: "For television and broadcast; looks right on SDR screens too (up to 1000 cd/m²).",
    save_as: "Save as",
    change: "Change …",
    export: "Make the video",
    export_done: "Done! Your video is saved.",
    export_again: "Make it again",
    frames_with_gaps: "{n} photos + {gaps} = {all}",
    gap_filled: ["stand-in", "stand-ins"],
    space_short: "Maybe not enough room: up to {need} needed, {free} free{tip}.",
    space_tip_4k: " (Full HD instead of 4K saves three quarters)",
    space_anyway: "Start anyway",
    queue_making: "Being made in the queue …",
    making: "Being made …",
    target_busy_short: "Another export is writing here …",
    measure_result: "Measure result",
    measure_result_tip: "Works the corrections out once more and measures the result with the finished look (curves, contrast, shadows): how much flicker is left, and where. Takes about twice as long as the correction.",
    measured_line: "Measured: {v} stops of flicker left frame to frame (visible from about 0.01).",
    measured_calm: "No visible flicker.",
    measured_slight: "Hardly visible flicker.",
    measured_visible: "Visible flicker.",
    measured_left_out: "({n} replaced or inserted frames not counted.)",
    measured_worst_calm: "Roughest stretch: frames {a}–{b} with {v} stops (does not show).",
    measured_wave: "A slow swell or kink around photos {a}–{b}: up to {v} stops (shows from about 0.05).",
    measured_wave_visible: "A visible slow swell.",
    measured_wave_drawn: "Your hand-drawn brightness bends there, as drawn.",
    measured_wave_nodefl: "\"Remove flicker\" is off: switch it on in step 4?",
    measured_wave_look: "Look there: is there a keyframe or a ramp's end?",
    measured_jump_nodefl: "\"Remove flicker\" is off: switch it on in step 4?",
    measured_worst_nodefl: "Roughest stretch: frames {a}–{b} with {v} stops (\"Remove flicker\" is off: switch it on in step 4?).",
    measured_step_head: "Jump between photo {a} and {b}: look there.",
    go_to_photo: "Go to photo {n}",
    to_step4: "Go to step 4",
    measured_spike_head: "Photo {a} stands out: look there.",
    measured_jump_size: "Jump: {v} stops.",
    measured_jump: "A keyframe there, a camera step or something bright such as the moon? Then choose an area to measure without it in step 4.",
    measured_worst: "Roughest stretch: frames {a}–{b} with {v} stops (look at it frame by frame: a keyframe, a camera step or something bright in the picture? An area to measure or stronger smoothing in step 4 may help).",
    verify_button: "Check delivery",
    verify_tip: "Reads every frame and compares it with SHA256SUMS.txt (as \"lapsewerk verify\"): after a copy to a stick or a NAS.",
    verify_ok: "Checked: all {n} frames are complete and undamaged.",
    verify_bad: "{n} file(s) differ: {names}. \"Go on\" makes changed or missing frames again; pictures not in the list are not part of the delivery.",
    verify_bad_other: "{n} file(s) differ: {names}. The frames there were made with other settings than those set now: \"Make the frames\" makes the whole sequence again.",
    verify_changed: "{name}: changed (no longer matches its checksum)",
    verify_cut: "{name}: cut short (much smaller than the other frames)",
    and_more: "… and {n} more",
    verify_missing: "{name}: missing",
    verify_extra: "{name}: extra picture, not in the list (not part of this delivery)",
    target_busy_measuring: "Another export ({who}) holds this folder and is still measuring (no frame yet): wait for it to end. Left over after a crash or restart? Delete the file .lapsewerk-lock in this folder.",
    queue_after: "Queue it to come after (waits, then goes on)",
    queue_after_replace: "Queue it to come after (waits, then replaces the frames)",
    target_beyond: "Frames past the end that are not part of this export: {names}. A new export removes them.",
    target_strangers: "Other pictures in the folder (kept, not part of this delivery): {names}",
    lock_who: "process {pid} {on}, started {since}",
    lock_seen: ", last active {seen}",
    lock_here: "on this computer",
    lock_on: "on {name}",
    target_busy: "Another export ({who}) is writing here now ({n} frames so far): wait for it to end. Left over after a crash or restart? Delete the file .lapsewerk-lock in this folder.",
    space_line: "Takes at most about {need} · {free} free",
    space_tight: "Room may be short: up to {need}, {free} free (an upper estimate: packed frames often need much less, the export can start all the same)",
    space_none: "No extra room needed (the frames there are replaced or continued) · {free} free",
    target_missing: "The destination folder is not there: {folder}. Is the drive plugged in? Otherwise choose another under “Change …”.",
    close_queue_title: "The queue is still rendering",
    close_queue_text: "If you close Lapsewerk now, the running export stops. Next time, it and the waiting ones are back in the queue and start when you say so. A frame sequence can be resumed; a movie starts again.",
    close_queue_keep: "Keep rendering",
    close_queue_quit: "Close anyway",
    no_photos: "There are no photos (JPEG, raw, TIFF or PNG) in this folder: “{folder}”. Choose the folder the pictures are in.",
    one_photo: "There is only one photo in this folder. A time-lapse needs a series: many photos of one scene a few seconds apart (about 250 for 10 s of video).",
    skipped_files: "Damaged and left out: {names}.",
    skipped_mid: "Inside the series their places get blends of the neighbouring frames (Finishing → fill gaps).",
    skipped_edge_one: "One was at the start or end of the series: the video is 1 frame shorter.",
    skipped_edge_many: "{n} were at the start or end of the series: the video is {n} frames shorter.",
    skipped_hint: "Damaged photos left out: {names}",
    export_left_out: "Without the damaged photos: {names}",
    coffee_line: "Lapsewerk is free. If it was worth a film to you:",
    coffee_link: "Buy me a coffee",
    export_done_frames: "Done! The frames of your video are saved.",
    frames_to_video: "This is a folder with one picture per video frame. To make a video of it: import the folder as an image sequence in DaVinci Resolve (free), Premiere or Final Cut and export a video. Lapsewerk makes movie files directly on a Mac.",
    proof_done_frames: "Test done (a folder of frames) – to look through; a playable test video is made on a Mac.",
    working_frames: "Making the frames",
    show_in_folder: "Show in folder",
    overwrite_question: "The file exists. Replace it?",
    needs_mac: "Lapsewerk writes movie files on a Mac only. Under “Advanced”, “As a frame sequence” works everywhere.",
};

/// `text` with `{n}` replaced by `n`.
#[must_use]
pub fn with_n(text: &str, n: impl std::fmt::Display) -> String {
    text.replace("{n}", &n.to_string())
}

/// A number with `decimals` places, with the decimal comma in German.
#[must_use]
pub fn num(lang: Lang, v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if lang == Lang::De { s.replace('.', ",") } else { s }
}

/// A duration as `m:ss` or `h:mm:ss`.
#[must_use]
pub fn clock(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// A size in bytes for people, with the language's decimal mark ("12,3 GB").
#[must_use]
pub fn bytes(lang: Lang, n: u64) -> String {
    let s = lapsewerk_video::space::gigabytes(n);
    if lang == Lang::De { s.replace('.', ",") } else { s }
}

/// What differs between a render there and this one ("timecode: there A, here B; keyframe 12
/// edited since …"), in the app's language.
#[must_use]
pub fn differs_words(lang: Lang, why: &str) -> String {
    if lang != Lang::De {
        return why.to_owned();
    }
    const NAMES: [(&str, &str); 18] = [
        ("frame name", "Name der Bilder"),
        ("size", "Größe"),
        ("frame rate", "Bildrate"),
        ("timecode", "Timecode"),
        ("curve", "Kennlinie"),
        ("frames", "Bilder"),
        ("deflicker", "Deflicker"),
        ("keep light", "Lichtanteil Tag↔Nacht"),
        ("hand-drawn brightness", "Handkurve"),
        ("in-between frames", "Zwischenbilder"),
        ("repairs", "Reparaturen"),
        ("passes", "Durchgänge"),
        ("position", "Ausschnitt"),
        ("hold blacks", "Schwarz halten"),
        ("stabilize", "Stabilisieren"),
        ("camera move", "Kamerafahrt"),
        ("dust", "Sensorstaub"),
        ("regional", "regionaler Deflicker"),
    ];
    why.split("; ")
        .map(|part| {
            if let Some(rest) = part.strip_prefix("keyframe ") {
                let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
                return if rest.contains("edited since") {
                    format!("Keyframe {n} seitdem bearbeitet (App, LightCraft oder von Hand)")
                } else if rest.contains("added since") {
                    format!("Keyframe {n} seitdem hinzugekommen")
                } else if rest.contains("taken away") {
                    format!("Keyframe {n} seitdem entfernt")
                } else {
                    part.to_owned()
                };
            }
            let Some((key, value)) = part.split_once(": ") else {
                return part.to_owned();
            };
            let name = NAMES.iter().find(|(e, _)| *e == key).map_or(key, |(_, d)| d);
            let value = value
                .replacen("there ", "dort ", 1)
                .replacen(", here ", ", hier ", 1)
                .replace("none", "keine")
                .replace("zoom ", "Zoom ")
                .replace(" to ", " auf ")
                .replace(", from ", ", von ")
                .replace(" on", " an")
                .replace(" off", " aus")
                .replace("pan from ", "Schwenk von ")
                .replace("pan ", "Schwenk ")
                .replace(" at Zoom ", " bei Zoom ");
            // A frame rate as "30 Bilder/s" (30000/1001 as 29,97), not "30/1".
            let value = if key == "frame rate" {
                value
                    .split(", ")
                    .map(|side| {
                        let (word, rate) = side.rsplit_once(' ').unwrap_or(("", side));
                        let shown = rate.split_once('/').and_then(|(n, d)| {
                            let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
                            (d > 0.0).then(|| {
                                let r = n / d;
                                if (r - r.round()).abs() < 1e-9 {
                                    format!("{r:.0}")
                                } else {
                                    format!("{r:.3}").trim_end_matches('0').to_owned()
                                }
                            })
                        });
                        match shown {
                            Some(r) => format!("{word} {r} Bilder/s").trim_start().to_owned(),
                            None => side.to_owned(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            } else if key == "size" {
                // As the format cards write a size: "1080 × 1920".
                value.replace('x', " × ")
            } else {
                value
            };
            // Decimal commas: "1.30" is "1,30" here (a digit on both sides of the point).
            let chars: Vec<char> = value.chars().collect();
            let value: String = chars
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let digits = i > 0
                        && chars.get(i - 1).is_some_and(char::is_ascii_digit)
                        && chars.get(i + 1).is_some_and(char::is_ascii_digit);
                    if *c == '.' && digits { ',' } else { *c }
                })
                .collect();
            format!("{name}: {value}")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A reason the command line gave (English) in the app's language: the common ones said plainly,
/// what follows the first colon kept as the detail; anything else as it came.
#[must_use]
pub fn reason(lang: Lang, e: &str) -> String {
    if e.contains("No space left") || e.contains("os error 28") {
        return lang.t().queue_disk_full.to_owned();
    }
    if lang != Lang::De {
        return e.to_owned();
    }
    // Another name than the frames there carry: both named, and the ways out.
    if let Some(rest) = e.split("its frames are named ").nth(1)
        && let Some((there, after)) = rest.split_once("_…, not ")
        && let Some((here, _)) = after.split_once("_…")
    {
        return format!(
            "Die Bilder im Zielordner heißen {there}_…, hier ist „{here}“ angegeben: das Feld „Name der Bilder“ leeren oder „{there}“ eintragen, um sie fortzusetzen oder zu ersetzen; für eine Lieferung „{here}“ einen anderen Ordner wählen"
        );
    }
    // Not enough room: with the figures.
    if let Some((need, free)) = e
        .split_once("needs about ")
        .and_then(|(_, r)| r.split_once(" and "))
        .and_then(|(need, r)| Some((need, r.split_once(" is free")?.0)))
    {
        return format!(
            "Zu wenig Platz am Ziel: der Export braucht etwa {need}, frei sind {free}. Platz schaffen und erneut starten (fertige Bilder bleiben) oder ein anderes Ziel wählen"
        );
    }
    if e.contains("rendered with other settings (") {
        let why = [
            ("another size or curve", "andere Größe oder Kennlinie"),
            (
                "another frame rate, timecode, look, keyframes or repairs",
                "andere Bildrate, Timecode, Look, Keyframes oder Reparaturen",
            ),
            (
                "other corrections",
                "anderer Ausgleich (Flackern, Tag ↔ Nacht oder Helligkeit)",
            ),
            (
                "settings file is missing",
                "die Einstellungsdatei dort fehlt oder ist unlesbar",
            ),
        ]
        .iter()
        .find(|(k, _)| e.contains(k))
        .map_or("andere Einstellungen", |(_, de)| de);
        // The values, when the message names them ("there 1920x1080 Srgb, here 1280x720 Srgb").
        let values = e
            .split_once("there ")
            .or_else(|| e.split_once("there: "))
            .and_then(|(_, r)| r.split_once("): ").map(|(v, _)| v))
            .map(|v| {
                format!(
                    "; dort {}",
                    v.trim_start_matches(": ")
                        .replace(", here ", ", hier ")
                        .replace("timecode", "Timecode")
                )
            })
            .unwrap_or_default();
        return format!(
            "Die Bilder dort wurden mit anderen Einstellungen erstellt ({why}{values}): „Neu rendern (ersetzt)“ statt fortsetzen"
        );
    }
    if let Some((_, frames)) = e.split_once("frames of another shape than ")
        && let Some((_, list)) = frames.split_once("): ")
    {
        let list = list
            .split(';')
            .next()
            .unwrap_or_default()
            .replace("frame ", "Bild ");
        return format!(
            "Fotos in einem anderen Format (hochkant und quer gemischt oder anders zugeschnitten): {list}. In LightCraft gleich zuschneiden oder diese Fotos aus dem Ordner nehmen"
        );
    }
    // Another render holds the folder: who, since when.
    if let Some((_, rest)) = e.split_once("another render (process ")
        && let Some((who, _)) = rest.split_once(") is writing")
    {
        let who = who
            .replace(" on this computer", " auf diesem Rechner")
            .replace(", since ", ", gestartet ")
            .replacen(" on ", " auf ", 1);
        return format!(
            "Ein anderer Export (Prozess {who}) schreibt gerade in diesen Ordner: warten, bis er fertig ist, oder ihn beenden. Liegt er nach einem Absturz oder Neustart nur noch da: die versteckte Datei .lapsewerk-lock dort löschen"
        );
    }
    let known: [(&str, &str); 29] = [
        (
            "holds Lapsewerk's frames named",
            "Im Zielordner liegen Lapsewerk-Bilder unter einem anderen Namen (Ordner umbenannt oder mit --name erstellt?): mit diesem Namen fortsetzen; zum Ersetzen diese Bilder erst aus dem Ordner verschieben; oder einen anderen Ordner wählen",
        ),
        (
            "its frames are named",
            "Die Bilder im Zielordner tragen einen anderen Namen als hier angegeben: mit ihrem Namen fortsetzen oder ersetzen; für eine Lieferung unter dem neuen Namen einen anderen Ordner wählen",
        ),
        (
            "holds a folder named",
            "Im Zielordner liegt ein Ordner mit dem Namen einer Lieferdatei (INFO.txt, HDR10.txt oder SHA256SUMS.txt): umbenennen oder verschieben",
        ),
        (
            "could not be taken",
            "Der Zielordner ließ sich nicht für diesen Export reservieren (.lapsewerk-lock): Schreibrechte prüfen oder einen anderen Ordner wählen",
        ),
        (
            "Operation not supported",
            "Der Datenträger am Ziel kann das nicht (z. B. eine Netzfreigabe oder ein Stick mit altem Dateisystem): einen anderen Ordner wählen",
        ),
        (
            "File exists",
            "Am Ziel liegt schon etwas, das kein Ordner für Bilder ist: ein anderes Ziel wählen",
        ),
        (
            "another render is starting",
            "Ein anderer Export beginnt gerade, in diesen Ordner zu schreiben: warten, bis er fertig ist",
        ),
        (
            "is a file, not a folder",
            "Am Ziel liegt eine Datei, kein Ordner: einen Ordner für die Bilder wählen",
        ),
        (
            "is not beside the app",
            "Die Warteschlange braucht das Programm „lapsewerk“ im selben Ordner wie die App: es dorthin legen (es gehört zur Installation)",
        ),
        (
            "system or home folder",
            "Das Ziel ist ein Systemordner oder dein Benutzerordner selbst: für die Bilder einen eigenen Ordner wählen",
        ),
        (
            "No such file or directory",
            "Ordner oder Datei nicht gefunden (Karte oder Platte nicht angeschlossen, verschoben oder umbenannt?)",
        ),
        (
            "another render (process",
            "Ein anderer Export schreibt gerade in diesen Ordner (aus der Warteschlange oder einem Terminal): warten, bis er fertig ist, oder ihn beenden. Nach einem Neustart ist das ein Überbleibsel: die versteckte Datei .lapsewerk-lock dort löschen",
        ),
        (
            "resuming works without",
            "Fortsetzen geht nicht mit Zwischenbildern, Entrauschen, Bewegungsunschärfe oder Sternbearbeitung (sie brauchen die Nachbarbilder): „Neu rendern (ersetzt)“",
        ),
        (
            "drive that is not mounted",
            "Das Ziel sieht aus wie eine nicht angeschlossene Platte (ein leerer Ordner auf der Systemplatte): Platte anschließen oder ein anderes Ziel wählen",
        ),
        (
            "series' photos or pictures not made by Lapsewerk",
            "Im Zielordner liegen die Fotos einer Serie oder Bilder, die Lapsewerk nicht gemacht hat: für die Bilder einen eigenen Ordner wählen",
        ),
        (
            "Operation not permitted",
            "Keine Schreibrechte im Zielordner: einen anderen Ordner wählen",
        ),
        (
            "is the photo folder itself",
            "Das Ziel ist der Fotoordner selbst: für die Bilder einen eigenen Ordner wählen (darin oder woanders)",
        ),
        (
            "is one of the photos",
            "Das Ziel ist eines der Fotos: ein anderes Ziel wählen",
        ),
        (
            "did not make there",
            "Im Zielordner liegen Dateien, die wie Bilder heißen, aber nicht von Lapsewerk dort erstellt wurden: sie bleiben, wie sie sind; einen leeren Ordner wählen",
        ),
        ("output path is empty", "Kein Ziel angegeben"),
        (
            "Permission denied",
            "Keine Schreibrechte im Zielordner: einen anderen Ordner wählen",
        ),
        (
            "Read-only file system",
            "Der Zielordner liegt auf einem schreibgeschützten Datenträger: einen anderen wählen",
        ),
        (
            "not a photo LightCraft can read",
            "Kein Foto, das Lapsewerk lesen kann",
        ),
        (
            "another file format",
            "Die Bilder dort haben ein anderes Dateiformat: mit „Neu rendern (ersetzt)“ ersetzen",
        ),
        (
            "is free there",
            "Zu wenig Platz am Ziel für den Export: Platz schaffen oder ein anderes Ziel wählen",
        ),
        (
            "rendered with other settings",
            "Die Bilder dort wurden mit anderen Einstellungen erstellt: „Neu rendern (ersetzt)“ statt fortsetzen",
        ),
        (
            "no light measurement",
            "HDR-Lichtwerte der vorhandenen Bilder fehlen: „Neu rendern (ersetzt)“ statt fortsetzen",
        ),
        (
            "is not there",
            "Der Zielordner ist nicht da (Platte nicht angeschlossen?)",
        ),
        (
            "holds",
            "Im Zielordner liegen schon Bilder: fortsetzen oder neu erstellen",
        ),
    ];
    known
        .iter()
        .find(|(k, _)| e.contains(k))
        .map_or_else(|| e.to_owned(), |(_, de)| (*de).to_owned())
}

/// Where a thank-you goes (PayPal.me): shown after a finished export and in the help.
pub const COFFEE_URL: &str = "https://paypal.me/TimKramarczyk";

/// Up to five names, then how many more: "a, b, c, d, e und 2 weitere".
#[must_use]
pub fn names_list(lang: Lang, names: &[String]) -> String {
    let mut s = names.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > 5 {
        let more = names.len() - 5;
        s.push_str(&match lang {
            Lang::De => format!(" und {more} weitere"),
            _ => format!(" and {more} more"),
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_differs_is_said_in_german() {
        assert_eq!(
            differs_words(
                Lang::De,
                "timecode: there 00:00:59;28, here 01:00:00;00; keyframe 12 edited since (in the app, LightCraft or by hand); hold blacks: there on, here off"
            ),
            "Timecode: dort 00:00:59;28, hier 01:00:00;00; Keyframe 12 seitdem bearbeitet (App, LightCraft oder von Hand); Schwarz halten: dort an, hier aus"
        );
        assert_eq!(
            differs_words(
                Lang::De,
                "camera move: there none, here zoom 1.00 to 1.30, from 0.50,0.50 to 0.40,0.50"
            ),
            "Kamerafahrt: dort keine, hier Zoom 1,00 auf 1,30, von 0,50,0,50 auf 0,40,0,50"
        );
    }

    #[test]
    fn a_frame_rate_is_said_in_frames_a_second() {
        assert_eq!(
            differs_words(Lang::De, "frame rate: there 30/1, here 30000/1001"),
            "Bildrate: dort 30 Bilder/s, hier 29,97 Bilder/s"
        );
    }

    #[test]
    fn another_frames_name_is_said_with_both_names() {
        let e =
            "out: its frames are named A1_…, not SHOT_…: give --name A1 to go on with them or replace them";
        let de = reason(Lang::De, e);
        assert!(de.contains("A1_…") && de.contains("„SHOT“"), "{de}");
    }

    #[test]
    fn command_line_reasons_are_said_in_german() {
        assert!(
            reason(Lang::De, "write failed: No space left on device (os error 28)")
                .starts_with("Datenträger voll")
        );
        assert!(
            reason(Lang::De, "out: its frames were rendered with other settings (…)")
                .contains("anderen Einstellungen")
        );
        assert_eq!(reason(Lang::De, "something new"), "something new");
        let r = reason(
            Lang::De,
            "out: its frames were rendered with other settings (another size or curve: there 1920x1080 Srgb, here 1280x720 Srgb): resuming would mix two renders; overwrite instead",
        );
        assert!(r.contains("dort 1920x1080 Srgb, hier 1280x720 Srgb"), "{r}");
        let r = reason(
            Lang::De,
            "out: another render (process 42 on mac-studio, since 2026-11-07 22:09) is writing into it now: wait",
        );
        assert!(
            r.contains("Prozess 42 auf mac-studio, gestartet 2026-11-07 22:09"),
            "{r}"
        );
        assert!(
            reason(
                Lang::De,
                "/x: the export needs about 249 MB and 126 MB is free there (--no-space-check renders anyway)"
            )
            .contains("braucht etwa 249 MB, frei sind 126 MB")
        );
        assert_eq!(reason(Lang::En, "something new"), "something new");
    }

    #[test]
    fn durations_read_like_clocks() {
        use std::time::Duration;
        assert_eq!(clock(Duration::from_secs(0)), "0:00");
        assert_eq!(clock(Duration::from_secs(75)), "1:15");
        assert_eq!(clock(Duration::from_secs(3725)), "1:02:05");
        assert_eq!(
            with_n(DE.exposure_steps_detail, 3),
            "3 gefunden, alle werden ausgeglichen"
        );
        assert_eq!(num(Lang::De, 5.62, 1), "5,6");
        assert_eq!(num(Lang::En, 5.62, 1), "5.6");
    }
}

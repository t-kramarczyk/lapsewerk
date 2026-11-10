//! Lapsewerk desktop app: a time-lapse in six numbered steps, with a project manager, previews and
//! the brightness curve; long work shows its progress and the time left.
//!
//! `lapsewerk-app` opens the window. Without a window or GPU (tests, CI, looking at the UI):
//! `lapsewerk-app --snapshot OUT.png [--size 1440x900] [--open FOLDER] [--step N] [--lang de|en] [--after S]
//!  [--mode play|compare|frames] [--export] [--verify] [--measure] [--proof] [--copy] [--brush]
//!  [--demo-edit] [--with-help] [--advanced] [--queue]`
//! opens `FOLDER`, works on to step `N` as if clicked through, waits until all work is done and
//! draws the UI with LightCraft's CPU renderer (`--after S`: after S seconds, work still running).

mod app;
mod curve;
mod help;
mod masks_ui;
mod pages;
mod preview;
mod queue;
mod recent;
mod sliders;
mod snapshot;
mod start;
mod text;
mod theme;
mod work;

use std::path::PathBuf;
use std::process::ExitCode;

struct Window {
    app: app::App,
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Closing while the queue renders: asked first (closing stops the running export).
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && self.app.queue.busy() && !self.app.close_anyway {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.app.ask_close = true;
        }
        self.app.frame(ui);
        if self.app.ask_close {
            let t = self.app.lang.t();
            let mut answer = None;
            egui::Modal::new(egui::Id::unique("close-with-queue")).show(&ctx, |ui| {
                ui.set_max_width(440.0);
                ui.label(egui::RichText::new(t.close_queue_title).strong().size(17.0));
                ui.add_space(6.0);
                ui.label(t.close_queue_text);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button(t.close_queue_keep).clicked() {
                        answer = Some(false);
                    }
                    if ui.button(t.close_queue_quit).clicked() {
                        answer = Some(true);
                    }
                });
            });
            match answer {
                Some(true) => {
                    self.app.ask_close = false;
                    self.app.close_anyway = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Some(false) => self.app.ask_close = false,
                None => {}
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.app.save_project_settings();
    }
}

struct Snapshot {
    out: PathBuf,
    size: [f32; 2],
    open: Option<PathBuf>,
    step: usize,
    lang: Option<text::Lang>,
    after: Option<f64>,
    export: bool,
    /// "Check delivery" in step 6.
    verify: bool,
    /// "Measure result" in step 5.
    measure: bool,
    proof: bool,
    copy: bool,
    brush: bool,
    /// Step 3 shows an edit in progress (a keyframe a little brighter, a mask) for screenshots.
    demo_edit: bool,
    help: bool,
    advanced: bool,
    queue: bool,
    mode: Option<app::PreviewMode>,
}

fn parse(args: &[String]) -> Result<Option<Snapshot>, String> {
    let Some(i) = args.iter().position(|a| a == "--snapshot") else {
        return Ok(None);
    };
    let value =
        |name: &str| -> Option<&String> { args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)) };
    let out = args.get(i + 1).ok_or("--snapshot needs a file")?;
    let size = match value("--size") {
        Some(s) => {
            let (w, h) = s.split_once('x').ok_or("--size expects WxH")?;
            [
                w.parse().map_err(|_| "--size width")?,
                h.parse().map_err(|_| "--size height")?,
            ]
        }
        None => [1440.0, 900.0],
    };
    Ok(Some(Snapshot {
        out: PathBuf::from(out),
        size,
        open: value("--open").map(PathBuf::from),
        step: value("--step")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1)
            .clamp(1, app::STEPS),
        lang: value("--lang").and_then(|l| text::Lang::from_code(l)),
        after: value("--after").and_then(|a| a.parse().ok()),
        export: args.iter().any(|a| a == "--export"),
        verify: args.iter().any(|a| a == "--verify"),
        measure: args.iter().any(|a| a == "--measure"),
        proof: args.iter().any(|a| a == "--proof"),
        copy: args.iter().any(|a| a == "--copy"),
        brush: args.iter().any(|a| a == "--brush"),
        demo_edit: args.iter().any(|a| a == "--demo-edit"),
        help: args.iter().any(|a| a == "--with-help"),
        advanced: args.iter().any(|a| a == "--advanced"),
        queue: args.iter().any(|a| a == "--queue"),
        mode: value("--mode").and_then(|m| match m.as_str() {
            "compare" => Some(app::PreviewMode::Compare),
            "frames" => Some(app::PreviewMode::Frames),
            "play" => Some(app::PreviewMode::Play),
            _ => None,
        }),
    }))
}

const USAGE: &str = "usage:
  lapsewerk-app [FOLDER]        the app (with FOLDER: that series opened)
  lapsewerk-app --snapshot OUT.png [--size 1440x900] [--open FOLDER] [--step N] [--lang de|en] [--after S]
                [--mode play|compare|frames] [--export] [--verify] [--measure] [--proof] [--copy]
                [--brush] [--demo-edit]
                [--with-help] [--advanced] [--queue]
                                a picture of the app without a screen (for docs and tests)
  lapsewerk-app --version | --help
the command line (render, analyze, import, calibrate) is `lapsewerk`; `lapsewerk --help`";

fn say(text: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        say(USAGE);
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        say(&format!("lapsewerk-app {}", env!("CARGO_PKG_VERSION")));
        return ExitCode::SUCCESS;
    }
    // Without --snapshot: nothing, or the folder of a series to open.
    let mut open_at_start = None;
    if !args.iter().any(|a| a == "--snapshot") {
        match args.as_slice() {
            [] => {}
            [folder] if !folder.starts_with('-') && std::path::Path::new(folder).exists() => {
                open_at_start = Some(PathBuf::from(folder));
            }
            [folder] if !folder.starts_with('-') => {
                eprintln!("lapsewerk-app: {folder} is not a folder");
                return ExitCode::FAILURE;
            }
            _ => {
                eprintln!("lapsewerk-app: unknown arguments {args:?}\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    match parse(&args) {
        Err(e) => {
            eprintln!("lapsewerk-app: {e}");
            ExitCode::FAILURE
        }
        Ok(Some(s)) => match snapshot::run(&s.out, s.size, s.after, |ctx| {
            let mut app = app::App::new(ctx);
            if let Some(l) = s.lang {
                app.lang = l;
            }
            if let Some(f) = &s.open {
                app.autopilot = Some(s.step);
                app.autopilot_export = s.export;
                app.autopilot_verify = s.verify;
                app.autopilot_measure = s.measure;
                app.autopilot_proof = s.proof;
                app.autopilot_copy = s.copy;
                app.autopilot_brush = s.brush;
                // A brush stroke or the copy dialog needs a keyframe being edited.
                app.autopilot_demo = s.demo_edit || s.brush || s.copy;
                app.help = s.help;
                app.open_advanced = s.advanced;
                app.autopilot_queue = s.queue;
                app.autopilot_mode = s.mode;
                app.open_folder(f);
            }
            app
        }) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("lapsewerk-app: {e}");
                ExitCode::FAILURE
            }
        },
        Ok(None) => {
            // The window's symbol (and its class, which the desktop file names): the packaged one.
            let mut viewport = egui::ViewportBuilder::default().with_app_id("lapsewerk-app");
            if let Ok(icon) =
                eframe::icon_data::from_png_bytes(include_bytes!("../../../packaging/lapsewerk.png"))
            {
                viewport = viewport.with_icon(icon);
            }
            let options = eframe::NativeOptions {
                viewport: viewport
                    .with_title("Lapsewerk")
                    .with_inner_size([1440.0, 900.0])
                    .with_min_inner_size([1100.0, 700.0])
                    .with_drag_and_drop(true),
                ..Default::default()
            };
            let run = eframe::run_native(
                "Lapsewerk",
                options,
                Box::new(move |cc| {
                    let mut app = app::App::new(&cc.egui_ctx);
                    if let Some(f) = &open_at_start {
                        app.open_folder(f);
                    }
                    Ok(Box::new(Window { app }))
                }),
            );
            match run {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("lapsewerk-app: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

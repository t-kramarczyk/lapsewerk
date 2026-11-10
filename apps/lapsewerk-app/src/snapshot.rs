//! The app drawn into an image without a window: an offscreen egui context driving the real UI,
//! tessellated and rasterized on the CPU by LightCraft's `softpaint`.

use std::path::Path;
use std::time::{Duration, Instant};

use lightcraft_ui_egui::softpaint::{self, TextureStore};

use crate::app::App;

/// Longest a snapshot waits for the app's work.
const TIMEOUT: Duration = Duration::from_secs(900);

/// Runs the app (made by `make`) frame by frame until all its work is done, then writes the last
/// frame as PNG.
/// With `after`, the frame at that time instead (work still running: progress, time left).
pub fn run(
    path: &Path,
    size: [f32; 2],
    after: Option<f64>,
    make: impl FnOnce(&egui::Context) -> App,
) -> Result<(), String> {
    let ctx = egui::Context::default();
    let mut app = make(&ctx);
    let mut textures = TextureStore::default();
    let started = Instant::now();
    let mut quiet = 0;
    let mut frame = 0u64;
    let px = [size[0] as usize, size[1] as usize];
    loop {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size.into())),
            time: Some(started.elapsed().as_secs_f64()),
            ..Default::default()
        };
        let out = ctx.run_ui(input, |ui| app.frame(ui));
        textures.apply(out.textures_delta);
        frame += 1;
        let done = app.idle() && app.autopilot_done();
        quiet = if done { quiet + 1 } else { 0 };
        let due = after.map_or(quiet >= 4, |a| started.elapsed().as_secs_f64() >= a);
        if due || started.elapsed() > TIMEOUT {
            let prims = ctx.tessellate(out.shapes, 1.0);
            let img = softpaint::paint(&prims, &textures, px, 1.0, egui::Color32::from_rgb(12, 12, 12));
            let [w, h] = img.size;
            let rgba: Vec<u8> = img
                .pixels
                .iter()
                .flat_map(|c| c.to_srgba_unmultiplied())
                .collect();
            eprintln!(
                "snapshot after {frame} frames, {:.1} s",
                started.elapsed().as_secs_f64()
            );
            return image::save_buffer(path, &rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)
                .map_err(|e| e.to_string());
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

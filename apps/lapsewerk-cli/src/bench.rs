//! `lapsewerk bench <folder>`: where a frame's render time goes (file decode vs develop, GPU vs CPU),
//! one frame at a time so the numbers are not mixed up by parallel jobs.

use std::time::Duration;

use lapsewerk_develop::{Output, OutputSpace, Sequence, run_timed};

pub fn run(args: &[String]) -> Result<(), String> {
    let mut folder = None;
    let mut frames = 6usize;
    let mut size = (3840usize, 3840usize);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--frames" => {
                frames = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--frames expects a number")?;
            }
            "--size" => {
                size = it
                    .next()
                    .and_then(|v| v.split_once('x'))
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .ok_or("--size expects WxH")?;
            }
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`")),
            s => folder = Some(std::path::PathBuf::from(s)),
        }
    }
    let folder = folder.ok_or("usage: lapsewerk bench <folder> [--frames N] [--size WxH]")?;
    let files = crate::frame_files(&folder)?;
    let mut seq = Sequence::open(&files).map_err(|e| e.to_string())?;
    seq.use_one_look(0).map_err(|e| e.to_string())?;
    let plan = seq
        .plan(lapsewerk_core::keyframes::Interpolation::Smooth)
        .map_err(|e| e.to_string())?;
    let n = frames.clamp(1, seq.len());
    let step = (seq.len() / n).max(1);
    let (mut read, mut decode) = (Duration::ZERO, Duration::ZERO);
    for k in 0..n {
        if let Some(f) = seq.frames().get(k * step)
            && let Some((r, d)) = lapsewerk_develop::raw_decode_timing(&f.path)
        {
            read += r;
            decode += d;
        }
    }
    println!(
        "{:<24} read {:>6.0} ms, raw decode {:>6.0} ms per frame",
        "file",
        read.as_secs_f64() * 1000.0 / n as f64,
        decode.as_secs_f64() * 1000.0 / n as f64
    );
    for (label, output, gpu) in [
        ("16-bit sRGB, GPU", Output::Display16(OutputSpace::Srgb), true),
        ("16-bit sRGB, CPU", Output::Display16(OutputSpace::Srgb), false),
        ("measuring 768 px, GPU", Output::LinearRec2020, true),
    ] {
        let (box_w, box_h) = if matches!(output, Output::LinearRec2020) {
            (768, 768)
        } else {
            size
        };
        let (mut load, mut develop, mut fallbacks) = (Duration::ZERO, Duration::ZERO, Vec::new());
        let mut out_size = (0, 0);
        for k in 0..n {
            let i = k * step;
            let job = seq
                .render_job(i, plan.settings(i, 0.0), box_w, box_h, output)
                .map_err(|e| e.to_string())?;
            let (r, t) = run_timed(job, gpu);
            let r = r?;
            if let Some(d) = &r.deep {
                out_size = (d.width, d.height);
            }
            load += t.load;
            develop += t.develop;
            fallbacks.extend(t.gpu_fallback);
        }
        let ms = |d: Duration| d.as_secs_f64() * 1000.0 / n as f64;
        println!(
            "{label:<24} {}×{}: load {:>6.0} ms, develop {:>6.0} ms per frame{}",
            out_size.0,
            out_size.1,
            ms(load),
            ms(develop),
            fallbacks
                .first()
                .map(|f| format!("  (GPU fell back: {f})"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

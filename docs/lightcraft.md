# LightCraft integration

Lapsewerk develops frames with [LightCraft](https://github.com/storytold/lightcraft) (MIT OR Apache-2.0),
used as a git dependency pinned to one commit of [`t-kramarczyk/lightcraft-lapsewerk`](https://github.com/t-kramarczyk/lightcraft-lapsewerk) (upstream
`5194cc2` plus additive patches, see its `LAPSEWERK.md`). Cargo fetches it with the git CLI (`.cargo/config.toml`).

## What is used

| LightCraft | Used for |
|---|---|
| `Session::new().with_fs()` + `library.import` | Opening frames in memory; reads each frame's XMP sidecar (`lc:settings`, else `crs:`) |
| `Session::develop_of`, `source_info` | A frame's settings and as-shot white balance |
| `develop::controls::{CONTROLS, get, set, indexed_instances}` | Numeric controls by id: what keyframes interpolate |
| `pipeline::local::normalize_wb` | Making white balance explicit (Kelvin) so a sequence keeps one |
| `files::camera_look_of`, `SequenceLook`, `fs_loader_for_sequence` (patch) | One look for every frame (a shared camera look, or none fitted to any frame); hot pixels repaired in the mosaic |
| `RawImage::repair_samples` (patch) | Replacing hot pixels with same-colour neighbours before demosaicing |
| `sidecar::parse_sidecar` (patches 5, 6) | A crs sidecar's crop and masks turned by the file's orientation (portrait raws) |
| `SourceRef::File::max_edge` | Loading the source at the size the output needs (+25 %, `no_binning_below`) |
| `lightcraft_raw::decode` + `normalized` | Raw sensor brightness, to verify exposure steps |
| `sidecar::find_sidecar` | Which frames are keyframes (have an XMP sidecar), by LightCraft's own naming rules |
| `Session::export_job` → `RenderJob::run` | Detached renders on worker threads: linear float Rec. 2020 for measuring, 16-bit sRGB for export |

Nothing is written by these calls: no LightCraft library, no sidecars.

## Rules

- **Exposure past ±5 stops.** `controls::set` clamps to the slider range; Lapsewerk adds ramp and deflicker
  offsets to `light.exposure` directly, which the pipeline renders exactly. Such values are never written back
  into a sidecar.
- **White balance.** Keyframes take white balance as explicit Kelvin/tint; "as shot" would follow the camera's
  auto white balance frame by frame and flicker.
- **Updating the pin.** Read LightCraft's changes since the pinned commit (develop model, `process` versions,
  render API), update `rev`, run `cargo test` and the analysis on the reference sequence, compare the CSV.
- **Video colour.** Frames are rendered as 16-bit sRGB (gamut-mapped by LightCraft) and written unchanged,
  tagged Rec. 709 primaries/matrix with the sRGB transfer curve (`--transfer 709` tags Rec. 709 instead).
- **HDR video.** `--hdr pq|hlg` renders with `hdr.enabled` and `hdr.max_ev` = the chosen headroom as
  `OutputDepth::F32Hdr` in Rec. 2020 (SDR white 1); the frames go through the stages as the sRGB curve of
  `value / peak` and the encoder's table makes PQ or HLG (`lapsewerk_core::hdr`, SDR white 203 cd/m², BT.2408).
  HEVC PQ carries HDR10 static metadata: mastering display Rec. 2020, D65, the peak; MaxCLL the peak (an upper
  bound: the metadata go in before the frames are rendered), MaxFALL 0 (not measured).
- **Renderer crashes.** Workers run each job under `catch_unwind`: a crash in one frame fails that frame
  (measuring) or the export (movie), never hangs it.

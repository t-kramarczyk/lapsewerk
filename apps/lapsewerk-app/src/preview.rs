//! Preview pictures: rendered on worker threads, kept as textures, the one looked at first.
//!
//! A picture is asked for by frame, kind and size. `get` returns it when it is there (else the
//! closest one there is, so the screen never goes blank while scrubbing) and queues it. The newest
//! request is served first: scrubbing shows the frame under the cursor at once, and a whole
//! sequence queued for playback fills in behind it, with its own progress and time left.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use lapsewerk_develop::{DevelopPlan, DevelopSettings, Sequence};
use lapsewerk_studio::Picture;

use crate::work::lock;

/// What a picture shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// As shot: the keyframes' look, no correction.
    Shot,
    /// As in the video: with the corrections of version `.0`.
    Video(u64),
    /// A keyframe being edited (settings version `.0`).
    Edit(u64),
}

/// Sizes (long edge, pixels).
pub const SMALL: usize = 480;
pub const LARGE: usize = 1280;

/// Frames held for playback at most: a longer sequence plays a window ahead of the cursor.
pub const PLAY_WINDOW: usize = 400;

/// Most memory the pictures take (about 1000 small frames).
pub const BUDGET: usize = 640 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub frame: usize,
    pub kind: Kind,
    pub edge: usize,
}

struct Request {
    key: Key,
    /// Exposure offset (Video).
    offset: f64,
    /// Settings (Edit).
    settings: Option<Arc<DevelopSettings>>,
}

/// What the workers share.
struct Queue {
    /// Newest first.
    jobs: VecDeque<Request>,
    queued: HashSet<Key>,
    stop: bool,
}

/// The sequence and plan pictures are rendered from.
pub struct Source {
    pub seq: Arc<Mutex<Sequence>>,
    pub plan: Arc<DevelopPlan>,
}

/// The brightness spread of a picture: 64 bins per channel, and the shares at black and at white.
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub bins: [Vec<u32>; 3],
    pub dark: f32,
    pub bright: f32,
}

impl Histogram {
    fn of(p: &Picture) -> Self {
        let mut bins = [vec![0u32; 64], vec![0u32; 64], vec![0u32; 64]];
        let (mut dark, mut bright, mut n) = (0u32, 0u32, 0u32);
        // Every third pixel is plenty for a histogram.
        for &[r, g, b, _] in p.rgba.as_chunks::<4>().0.iter().step_by(3) {
            for (bins, v) in bins.iter_mut().zip([r, g, b]) {
                if let Some(bin) = bins.get_mut(usize::from(v) / 4) {
                    *bin += 1;
                }
            }
            let top = r.max(g).max(b);
            if top >= 254 {
                bright += 1;
            }
            if top <= 1 {
                dark += 1;
            }
            n += 1;
        }
        let n = n.max(1) as f32;
        Self {
            bins,
            dark: dark as f32 / n,
            bright: bright as f32 / n,
        }
    }
}

pub struct Previews {
    textures: HashMap<Key, egui::TextureHandle>,
    /// Of the large pictures (the one looked at).
    histograms: HashMap<Key, Histogram>,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    source: Arc<Mutex<Option<Source>>>,
    done_rx: Receiver<(Key, Result<Picture, String>)>,
    workers: Vec<std::thread::JoinHandle<()>>,
    /// Bulk work (a whole sequence for playback): its kind, how many, since when.
    pub bulk: Option<(Kind, usize, usize, Instant)>,
    failed: HashSet<Key>,
    /// Most bytes the pictures may take ([`BUDGET`]).
    budget: usize,
}

impl Previews {
    #[must_use]
    pub fn new(ctx: &egui::Context) -> Self {
        let queue = Arc::new((
            Mutex::new(Queue {
                jobs: VecDeque::new(),
                queued: HashSet::new(),
                stop: false,
            }),
            Condvar::new(),
        ));
        let source: Arc<Mutex<Option<Source>>> = Arc::new(Mutex::new(None));
        let (tx, rx) = channel();
        let threads = std::thread::available_parallelism()
            .map_or(2, std::num::NonZero::get)
            .clamp(1, 4);
        let workers = (0..threads)
            .filter_map(|_| {
                let (q, src, tx, ctx): (_, _, Sender<_>, _) =
                    (Arc::clone(&queue), Arc::clone(&source), tx.clone(), ctx.clone());
                std::thread::Builder::new()
                    .name("lapsewerk-preview".into())
                    .spawn(move || worker(&q, &src, &tx, &ctx))
                    .ok()
            })
            .collect();
        Self {
            textures: HashMap::new(),
            histograms: HashMap::new(),
            queue,
            source,
            done_rx: rx,
            workers,
            bulk: None,
            failed: HashSet::new(),
            budget: BUDGET,
        }
    }

    /// Renders from `source` from now on (a new plan: pictures of the old one are dropped).
    pub fn set_source(&mut self, source: Option<Source>) {
        *lock(&self.source) = source;
        self.clear();
    }

    /// The histogram of frame `frame`'s large picture of `kind`, else of the newest of that frame.
    #[must_use]
    pub fn histogram_near(&self, frame: usize, kind: Kind) -> Option<Histogram> {
        let exact = Key {
            frame,
            kind,
            edge: LARGE,
        };
        self.histograms.get(&exact).cloned().or_else(|| {
            self.histograms
                .iter()
                .filter(|(k, _)| k.frame == frame)
                .max_by_key(|(k, _)| match k.kind {
                    Kind::Edit(v) | Kind::Video(v) => v,
                    Kind::Shot => 0,
                })
                .map(|(_, h)| h.clone())
        })
    }

    /// Forgets every picture and request.
    pub fn clear(&mut self) {
        self.textures.clear();
        self.histograms.clear();
        self.failed.clear();
        self.bulk = None;
        let (q, _) = &*self.queue;
        let mut q = lock(q);
        q.jobs.clear();
        q.queued.clear();
    }

    /// Forgets the pictures of `kind` (their settings changed).
    pub fn drop_kind(&mut self, keep: impl Fn(Kind) -> bool) {
        self.textures.retain(|k, _| keep(k.kind));
        self.histograms.retain(|k, _| keep(k.kind));
        self.failed.retain(|k| keep(k.kind));
        let (q, _) = &*self.queue;
        let mut q = lock(q);
        q.jobs.retain(|r| keep(r.key.kind));
        let jobs: HashSet<Key> = q.jobs.iter().map(|r| r.key).collect();
        q.queued = jobs;
        if self.bulk.is_some_and(|(k, ..)| !keep(k)) {
            self.bulk = None;
        }
    }

    /// Takes in finished pictures (call every frame).
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((key, result)) = self.done_rx.try_recv() {
            match result {
                Ok(p) => {
                    if key.edge == LARGE {
                        self.histograms.insert(key, Histogram::of(&p));
                    }
                    let image = egui::ColorImage::from_rgba_unmultiplied([p.width, p.height], &p.rgba);
                    let name = format!("preview-{}-{:?}-{}", key.frame, key.kind, key.edge);
                    let tex = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
                    self.textures.insert(key, tex);
                }
                Err(e) => {
                    if std::env::var_os("LAPSEWERK_DEBUG").is_some() {
                        eprintln!("preview {key:?}: {e}");
                    }
                    self.failed.insert(key);
                }
            }
        }
        if let Some((kind, total, done, started)) = self.bulk {
            let have = self
                .textures
                .keys()
                .chain(self.failed.iter())
                .filter(|k| k.kind == kind && k.edge == SMALL)
                .count();
            self.bulk = (have < total).then_some((kind, total, have.max(done), started));
        }
    }

    /// The picture for `key` if there is one (queued otherwise; the newest request comes first).
    pub fn get(
        &mut self,
        key: Key,
        offset: f64,
        settings: Option<Arc<DevelopSettings>>,
    ) -> Option<&egui::TextureHandle> {
        if !self.textures.contains_key(&key) && !self.failed.contains(&key) {
            self.request(key, offset, settings, true);
        }
        self.textures.get(&key)
    }

    /// The picture for `key`, or the closest one there is: the same frame smaller or as shot.
    pub fn get_or_near(
        &mut self,
        key: Key,
        offset: f64,
        settings: Option<Arc<DevelopSettings>>,
    ) -> Option<egui::TextureHandle> {
        if let Some(t) = self.get(key, offset, settings) {
            return Some(t.clone());
        }
        let near = [
            Key { edge: SMALL, ..key },
            Key {
                edge: LARGE,
                kind: Kind::Shot,
                ..key
            },
            Key {
                edge: SMALL,
                kind: Kind::Shot,
                ..key
            },
        ];
        near.iter().find_map(|k| self.textures.get(k).cloned())
    }

    /// The picture for `key` if there is one; queued after what is asked for now otherwise
    /// (thumbnails that must not hold up the picture looked at).
    pub fn want(&mut self, key: Key, offset: f64) -> Option<&egui::TextureHandle> {
        if !self.textures.contains_key(&key) && !self.failed.contains(&key) {
            self.request(key, offset, None, false);
        }
        self.textures.get(&key)
    }

    /// Queues the frames to play (small), after what is asked for now: every frame of a short
    /// sequence, the [`PLAY_WINDOW`] frames from `cursor` on of a long one (so memory stays bounded).
    pub fn want_play(&mut self, kind: Kind, offsets: &[f64], frames: usize, cursor: usize) {
        if frames == 0 || self.bulk.is_some_and(|(k, ..)| k == kind) {
            return;
        }
        let window: Vec<usize> = if frames <= PLAY_WINDOW {
            (0..frames).collect()
        } else {
            (0..PLAY_WINDOW).map(|k| (cursor + k) % frames).collect()
        };
        let missing: Vec<usize> = window
            .iter()
            .copied()
            .filter(|&frame| {
                let key = Key {
                    frame,
                    kind,
                    edge: SMALL,
                };
                !self.textures.contains_key(&key) && !self.failed.contains(&key)
            })
            .collect();
        // All there already: nothing to do (and nothing to report as running).
        if missing.is_empty() {
            return;
        }
        for &frame in &missing {
            let key = Key {
                frame,
                kind,
                edge: SMALL,
            };
            self.request(key, offsets.get(frame).copied().unwrap_or(0.0), None, false);
        }
        self.bulk = Some((kind, window.len(), window.len() - missing.len(), Instant::now()));
    }

    /// Keeps the pictures within their budget ([`BUDGET`] bytes): those furthest ahead of `cursor` (in playing
    /// order, so the frames just played go first) are let go; the frame at the cursor stays.
    pub fn trim(&mut self, cursor: usize, frames: usize) {
        let size = |t: &egui::TextureHandle| t.size()[0] * t.size()[1] * 4;
        let mut bytes: usize = self.textures.values().map(size).sum();
        if bytes <= self.budget {
            return;
        }
        let n = frames.max(1);
        let mut keys: Vec<(usize, Key)> = self
            .textures
            .keys()
            .map(|k| ((k.frame + n - cursor % n) % n, *k))
            .collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.0));
        for (ahead, key) in keys {
            if bytes <= self.budget * 9 / 10 || ahead == 0 {
                break;
            }
            if let Some(t) = self.textures.remove(&key) {
                bytes = bytes.saturating_sub(size(&t));
            }
        }
    }

    /// Playback progress: done, total, time left.
    #[must_use]
    pub fn bulk_status(&self) -> Option<(usize, usize, Option<std::time::Duration>)> {
        self.bulk.map(|(_, total, done, started)| {
            (
                done,
                total,
                lapsewerk_studio::estimate(started.elapsed(), done, total),
            )
        })
    }

    /// Is anything still being rendered?
    #[must_use]
    pub fn busy(&self) -> bool {
        let (q, _) = &*self.queue;
        // `queued` holds the pictures being rendered too, not only those waiting.
        !lock(q).queued.is_empty() || self.bulk.is_some()
    }

    fn request(&mut self, key: Key, offset: f64, settings: Option<Arc<DevelopSettings>>, urgent: bool) {
        let (q, cv) = &*self.queue;
        let mut q = lock(q);
        if q.queued.contains(&key) {
            if urgent && let Some(pos) = q.jobs.iter().position(|r| r.key == key) {
                // Asked for again: to the front.
                if let Some(r) = q.jobs.remove(pos) {
                    q.jobs.push_front(r);
                }
            }
            return;
        }
        q.queued.insert(key);
        let r = Request {
            key,
            offset,
            settings,
        };
        if urgent {
            q.jobs.push_front(r);
        } else {
            q.jobs.push_back(r);
        }
        cv.notify_one();
    }
}

impl Drop for Previews {
    fn drop(&mut self) {
        let (q, cv) = &*self.queue;
        lock(q).stop = true;
        cv.notify_all();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

fn worker(
    queue: &(Mutex<Queue>, Condvar),
    source: &Mutex<Option<Source>>,
    tx: &Sender<(Key, Result<Picture, String>)>,
    ctx: &egui::Context,
) {
    let (q, cv) = queue;
    loop {
        let req = {
            let mut g = lock(q);
            loop {
                if g.stop {
                    return;
                }
                if let Some(r) = g.jobs.pop_front() {
                    break r;
                }
                g = cv.wait(g).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        // The job is made with the sequence held (quick), and run without it.
        let job = {
            let src = lock(source);
            let Some(src) = src.as_ref() else {
                lock(q).queued.remove(&req.key);
                continue;
            };
            let mut seq = lock(&src.seq);
            match (&req.key.kind, &req.settings) {
                (Kind::Edit(_), Some(s)) => {
                    lapsewerk_studio::preview_job_with(&mut seq, req.key.frame, (**s).clone(), req.key.edge)
                }
                (Kind::Video(_), _) => lapsewerk_studio::preview_job(
                    &mut seq,
                    &src.plan,
                    req.key.frame,
                    req.offset,
                    req.key.edge,
                ),
                _ => lapsewerk_studio::preview_job(&mut seq, &src.plan, req.key.frame, 0.0, req.key.edge),
            }
        };
        let debug = std::env::var_os("LAPSEWERK_DEBUG").is_some();
        let started = std::time::Instant::now();
        if debug {
            eprintln!("preview {:?}: start", req.key);
        }
        let result = job.and_then(lapsewerk_studio::run_preview);
        if debug {
            eprintln!("preview {:?}: {:?}", req.key, started.elapsed());
        }
        lock(q).queued.remove(&req.key);
        if tx.send((req.key, result)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_sequences_play_from_a_window_and_memory_stays_bounded() {
        let ctx = egui::Context::default();
        let mut p = Previews::new(&ctx);
        // 100 frames of 100 × 100 (40 000 bytes each); room for 25.
        p.budget = 25 * 40_000;
        for frame in 0..100 {
            let image = egui::ColorImage::filled([100, 100], egui::Color32::GRAY);
            let tex = ctx.load_texture(format!("t{frame}"), image, egui::TextureOptions::LINEAR);
            p.textures.insert(
                Key {
                    frame,
                    kind: Kind::Video(1),
                    edge: SMALL,
                },
                tex,
            );
        }
        p.trim(40, 100);
        let kept: Vec<usize> = {
            let mut v: Vec<usize> = p.textures.keys().map(|k| k.frame).collect();
            v.sort_unstable();
            v
        };
        assert!(kept.len() <= 25, "{}", kept.len());
        // What is about to play stays: the cursor and the frames right after it.
        assert!(
            kept.contains(&40) && kept.contains(&41) && kept.contains(&50),
            "{kept:?}"
        );
        assert!(!kept.contains(&39), "the frame just played goes first");
        // A long sequence asks for a window from the cursor on, not every frame.
        p.clear();
        // Workers stopped, so the queue stays as asked.
        {
            let (q, cv) = &*p.queue;
            lock(q).stop = true;
            cv.notify_all();
        }
        for w in std::mem::take(&mut p.workers) {
            let _ = w.join();
        }
        p.want_play(Kind::Video(2), &[], PLAY_WINDOW * 3, 10);
        let (q, _) = &*p.queue;
        let queued: Vec<usize> = lock(q).jobs.iter().map(|r| r.key.frame).collect();
        assert_eq!(queued.len(), PLAY_WINDOW);
        assert!(queued.contains(&10) && !queued.contains(&9));
    }
}

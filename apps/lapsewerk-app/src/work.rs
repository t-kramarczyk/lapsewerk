//! Long work off the UI thread: one task at a time (open, analyse, correct, export), reporting its
//! step, how far it is and the time left, cancellable. The UI polls [`Task::status`] every frame
//! and is woken (`request_repaint`) whenever something changes.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lapsewerk_core::sequence::Timing;
use lapsewerk_develop::{DevelopPlan, Sequence};
use lapsewerk_studio::{Analysis, Corrected, Reporter, Step};

/// How far back the pace for the time left is taken.
const PACE_WINDOW: Duration = Duration::from_secs(45);

/// Time left of the current step: at the pace of the last [`PACE_WINDOW`] once that spans a few
/// seconds and some frames, before that at the average since the step started.
fn time_left(i: &Inner) -> Option<Duration> {
    if let Some(&(t0, d0)) = i.recent.front() {
        let span = t0.elapsed().as_secs_f64();
        let done = i.done.saturating_sub(d0);
        if span >= 5.0 && done >= 4 && i.done <= i.total {
            return Duration::try_from_secs_f64(span / done as f64 * (i.total - i.done) as f64).ok();
        }
    }
    lapsewerk_studio::estimate(i.step_started.elapsed(), i.done, i.total)
}

/// What a task is doing, for the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doing {
    Opening,
    Step(Step),
}

impl Doing {
    /// Index into `Texts::working`.
    #[must_use]
    pub fn text_index(self) -> usize {
        match self {
            Self::Opening => 0,
            Self::Step(Step::Measure) => 1,
            Self::Step(Step::Verify) => 2,
            Self::Step(Step::Check) => 3,
            Self::Step(Step::Dust) => 4,
            Self::Step(Step::Render) => 5,
            Self::Step(Step::Calibrate) => 7,
        }
    }
}

/// A snapshot of a task's progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub doing: Doing,
    pub done: usize,
    pub total: usize,
    /// Of the current step; `None` until it can be told.
    pub left: Option<Duration>,
    /// Why the analysis measures again, as noted ("keyframes changed … measured again").
    pub again: Option<String>,
    /// How often the current step began anew (the check pass measures twice: 1, then 2).
    pub round: usize,
}

impl Status {
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| {
            let f = (self.done as f32 / self.total as f32).clamp(0.0, 1.0);
            // The check pass measures twice: one bar over both rounds.
            if self.doing == Doing::Step(Step::Check) {
                (self.round.clamp(1, 2) as f32 - 1.0 + f) / 2.0
            } else {
                f
            }
        })
    }
}

/// A sequence just opened.
pub struct Opened {
    pub seq: Sequence,
    pub plan: DevelopPlan,
    pub timing: Option<Timing>,
}

/// How a task ended.
pub enum Outcome {
    Opened(Box<Opened>),
    Analysed(Box<Analysis>),
    Corrected(Box<Corrected>),
    Exported(PathBuf),
    Calibrated(lapsewerk_develop::calibrate::Calibrated),
    /// A delivery checked against its checksums: frames as written, and those that differ.
    Verified(usize, Vec<String>),
    Failed(String),
    Cancelled,
}

struct Inner {
    doing: Doing,
    done: usize,
    total: usize,
    step_started: Instant,
    /// (when, done) of the step's recent progress: the time left follows the recent pace, not the
    /// average since the start (the first frames are slow while the thumbnails still load).
    recent: VecDeque<(Instant, usize)>,
    round: usize,
    notes: Vec<String>,
    outcome: Option<Outcome>,
}

/// Shared between the task's thread and the UI; the task's [`Reporter`].
pub struct Shared {
    cancel: AtomicBool,
    inner: Mutex<Inner>,
    ctx: egui::Context,
}

impl Shared {
    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut g)
    }
}

impl Reporter for Shared {
    fn progress(&self, step: Step, done: usize, total: usize) {
        self.with(|i| {
            if i.doing != Doing::Step(step) {
                i.round = 1;
            } else if done == 0 && i.done > 0 {
                // The same step from the start again (the check pass checking its corrections).
                i.round += 1;
            }
            let now = Instant::now();
            if i.doing != Doing::Step(step) || done == 0 {
                i.doing = Doing::Step(step);
                i.step_started = now;
                i.recent.clear();
            }
            i.recent.push_back((now, done));
            // Keep one sample older than the window, so the pace spans all of it.
            while i.recent.len() > 2 && i.recent.get(1).is_some_and(|(t, _)| now.duration_since(*t) > PACE_WINDOW) {
                i.recent.pop_front();
            }
            i.done = done;
            i.total = total;
        });
        self.ctx.request_repaint();
    }

    fn note(&self, line: &str) {
        self.with(|i| i.notes.push(line.to_owned()));
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// A task running on its own thread.
pub struct Task {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Task {
    /// Runs `work` on a new thread; it reports through the [`Shared`] it is given.
    pub fn spawn(ctx: &egui::Context, work: impl FnOnce(&Shared) -> Outcome + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            cancel: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                doing: Doing::Opening,
                done: 0,
                total: 0,
                step_started: Instant::now(),
                recent: VecDeque::new(),
                round: 1,
                notes: Vec::new(),
                outcome: None,
            }),
            ctx: ctx.clone(),
        });
        let s = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("lapsewerk-task".into())
            .spawn(move || {
                // A crash in the work becomes an error message, never a dead app.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&s)))
                    .unwrap_or_else(|_| Outcome::Failed("internal error (crash in the task)".into()));
                let outcome = match outcome {
                    Outcome::Failed(e) if e == lapsewerk_studio::CANCELLED => Outcome::Cancelled,
                    o => o,
                };
                s.with(|i| i.outcome = Some(outcome));
                s.ctx.request_repaint();
            })
            .ok();
        if thread.is_none() {
            shared.with(|i| i.outcome = Some(Outcome::Failed("cannot start a thread".into())));
        }
        Self { shared, thread }
    }

    #[must_use]
    pub fn status(&self) -> Status {
        self.shared.with(|i| Status {
            doing: i.doing,
            done: i.done,
            total: i.total,
            left: time_left(i).map(|left| {
                // The check pass's first round: its second (as long) follows, counted in.
                if i.doing == Doing::Step(Step::Check) && i.round == 1 {
                    left * 2 + i.step_started.elapsed()
                } else {
                    left
                }
            }),
            round: i.round,
            again: i
                .notes
                .iter()
                .find(|n| n.starts_with("analysis:") && n.contains("measured again"))
                .cloned(),
        })
    }

    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn cancelling(&self) -> bool {
        self.shared.cancel.load(Ordering::Relaxed)
    }

    /// The outcome once the task has ended (taken: returns it once), with the notes it made.
    pub fn take_outcome(&mut self) -> Option<(Outcome, Vec<String>)> {
        let out = self
            .shared
            .with(|i| i.outcome.take().map(|o| (o, std::mem::take(&mut i.notes))));
        if out.is_some()
            && let Some(t) = self.thread.take()
        {
            let _ = t.join();
        }
        out
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Locks a mutex, recovering from a panic elsewhere.
pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// [`lock`] without waiting: `None` while another thread holds it (an export holds the sequence
/// for its whole run). What the screen draws every frame uses this, so the window never waits.
pub fn try_lock<T>(m: &Mutex<T>) -> Option<std::sync::MutexGuard<'_, T>> {
    match m.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(t: &mut Task) -> (Outcome, Vec<String>) {
        for _ in 0..500 {
            if let Some(o) = t.take_outcome() {
                return o;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("task did not end");
    }

    #[test]
    fn reports_progress_notes_and_cancelling() {
        let ctx = egui::Context::default();
        let gate = Arc::new(AtomicBool::new(false));
        let g = Arc::clone(&gate);
        let mut t = Task::spawn(&ctx, move |rep| {
            rep.progress(Step::Render, 0, 10);
            rep.progress(Step::Render, 4, 10);
            rep.note("hello");
            while !g.load(Ordering::Relaxed) {
                if rep.cancelled() {
                    return Outcome::Failed(lapsewerk_studio::CANCELLED.into());
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Outcome::Exported(PathBuf::from("x.mov"))
        });
        for _ in 0..200 {
            if t.status().done == 4 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let s = t.status();
        assert_eq!((s.doing, s.done, s.total), (Doing::Step(Step::Render), 4, 10));
        assert_eq!(s.fraction(), Some(0.4));
        assert!(s.left.is_some());
        t.cancel();
        let (outcome, notes) = wait(&mut t);
        assert!(matches!(outcome, Outcome::Cancelled));
        assert_eq!(notes, ["hello"]);
        assert!(t.take_outcome().is_none(), "the outcome is given once");
    }

    #[test]
    fn a_crash_becomes_an_error() {
        let ctx = egui::Context::default();
        #[allow(clippy::panic)]
        let mut t = Task::spawn(&ctx, |_| panic!("boom"));
        assert!(matches!(wait(&mut t).0, Outcome::Failed(_)));
    }
}

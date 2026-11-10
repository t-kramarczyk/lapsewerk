//! The render queue: exports lined up to run one after another, in the background, while the app
//! goes on (another project, or nothing at all). Each runs the command line (`lapsewerk render`,
//! beside the app) with the settings frozen when it was queued, so later edits do not change it,
//! and a stopped app or a crash in one render leaves the others alone.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

/// A queued export.
pub struct Job {
    /// The project's name, as shown.
    pub name: String,
    /// The photo folder.
    pub folder: PathBuf,
    /// Where it goes (movie or folder of frames).
    pub out: PathBuf,
    /// `lapsewerk render` arguments after the folder.
    pub args: Vec<String>,
    pub state: State,
}

/// Where a job stands.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Waiting,
    /// Running: the step ("measured", "rendered" …) and how far (0..1).
    Running(String, f32),
    Done,
    Failed(String),
    Stopped,
    /// Waiting when the app was closed: kept, and started again only when asked.
    Paused,
}

/// What the running render reported, shared with the thread reading its output.
#[derive(Default)]
struct Shared {
    step: String,
    fraction: f32,
    /// Done and all of the current step (frames).
    counts: (usize, usize),
    /// When the current step began (for the time left).
    since: Option<std::time::Instant>,
    /// The last error line.
    error: Option<String>,
}

struct Running {
    index: usize,
    child: Child,
    shared: Arc<Mutex<Shared>>,
    /// The thread reading the render's output (done once the output ends).
    reader: Option<std::thread::JoinHandle<()>>,
    stop_asked: bool,
}

/// The queue.
#[derive(Default)]
pub struct Queue {
    pub jobs: Vec<Job>,
    running: Option<Running>,
    /// A job that found its folder held by another render: tried again from this time on.
    later: Option<(usize, std::time::Instant)>,
    /// A job that waited for another render and now goes on with its frames (`--resume` in
    /// place of `--overwrite`): its own arguments, back if going on is refused.
    resumed: Option<(usize, Vec<String>)>,
    /// What was last written to the settings folder ([`Queue::save_at`]).
    saved: String,
    /// The queue kept from last time could not be read: put aside under this name, to be said.
    pub damaged: Option<PathBuf>,
}

/// The file in the app's settings folder that keeps the jobs not done over a restart.
const FILE: &str = "queue.json";

/// The command line beside the app (`lapsewerk`, `lapsewerk.exe`).
#[must_use]
pub fn cli_path() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let name = if cfg!(windows) {
        "lapsewerk.exe"
    } else {
        "lapsewerk"
    };
    let p = dir.join(name);
    p.is_file().then_some(p)
}

/// A line of the command line's progress (`progress rendered 12 300`): the step and how far.
fn parse_progress(line: &str) -> Option<(String, usize, usize)> {
    let mut parts = line.strip_prefix("progress ")?.split_whitespace();
    let step = parts.next()?.to_owned();
    let done: usize = parts.next()?.parse().ok()?;
    let total: usize = parts.next()?.parse().ok()?;
    Some((step, done.min(total), total))
}

/// The running job, for a status line.
pub struct Progress {
    pub name: String,
    /// Its step, as the command line names it ("rendered").
    pub step: String,
    /// How far the step is (0..1), and in frames.
    pub fraction: f32,
    pub done: usize,
    pub total: usize,
    /// The time the step still needs (from the rate so far).
    pub left: Option<std::time::Duration>,
    /// Jobs waiting after it.
    pub waiting: usize,
}

impl Queue {
    /// Adds a job (it runs when the ones before it are done).
    pub fn add(&mut self, name: String, folder: PathBuf, out: PathBuf, args: Vec<String>) {
        self.jobs.push(Job {
            name,
            folder,
            out,
            args,
            state: State::Waiting,
        });
    }

    /// The running job, for a status line: its name, step, how far (0..1), the time its step
    /// still needs (from the rate so far), and how many wait after it.
    #[must_use]
    pub fn running(&self) -> Option<Progress> {
        let r = self.running.as_ref()?;
        let job = self.jobs.get(r.index)?;
        let s = r.shared.lock().unwrap_or_else(PoisonError::into_inner);
        let left = s.since.filter(|_| s.fraction > 0.02).map(|t| {
            let spent = t.elapsed().as_secs_f64();
            std::time::Duration::from_secs_f64(spent * f64::from(1.0 - s.fraction) / f64::from(s.fraction))
        });
        let waiting = self.jobs.iter().filter(|j| j.state == State::Waiting).count();
        Some(Progress {
            name: job.name.clone(),
            step: s.step.clone(),
            fraction: s.fraction,
            done: s.counts.0,
            total: s.counts.1,
            left,
            waiting,
        })
    }

    /// Whether a job is running or waiting.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.jobs
            .iter()
            .any(|j| matches!(j.state, State::Waiting | State::Running(..)))
    }

    /// Whether `out` is queued or being made (another export there would collide).
    #[must_use]
    pub fn has_target(&self, out: &Path) -> bool {
        self.jobs
            .iter()
            .any(|j| j.out == out && matches!(j.state, State::Waiting | State::Running(..) | State::Paused))
    }

    /// The jobs not done, as kept over a restart: one waiting or running then is paused or stopped
    /// now (the app stopped its render when it closed), a failed one keeps its reason.
    fn kept(&self) -> serde_json::Value {
        let jobs: Vec<serde_json::Value> = self
            .jobs
            .iter()
            .filter_map(|j| {
                let state = match &j.state {
                    State::Done => return None,
                    State::Waiting | State::Paused => "paused".to_owned(),
                    State::Running(..) | State::Stopped => "stopped".to_owned(),
                    State::Failed(e) => format!("failed:{e}"),
                };
                Some(serde_json::json!({
                    "name": j.name,
                    "folder": j.folder.to_string_lossy(),
                    "out": j.out.to_string_lossy(),
                    "args": j.args,
                    "state": state,
                }))
            })
            .collect();
        serde_json::json!({ "jobs": jobs })
    }

    /// Writes the jobs not done to `dir` when they changed since the last time (atomically;
    /// errors are ignored: a lost list only loses the convenience, never a render).
    pub fn save_at(&mut self, dir: Option<&Path>) {
        let Some(dir) = dir else { return };
        let Ok(text) = serde_json::to_string_pretty(&self.kept()) else {
            return;
        };
        if text == self.saved {
            return;
        }
        let _ = std::fs::create_dir_all(dir);
        let tmp = dir.join(format!(".queue.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, &text).is_ok() && std::fs::rename(&tmp, dir.join(FILE)).is_ok() {
            self.saved = text;
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// The queue kept in `dir` from the last time the app ran: its jobs paused or stopped, none
    /// started until asked (an empty queue when there is none or it cannot be read).
    #[must_use]
    pub fn load_at(dir: Option<&Path>) -> Self {
        let mut q = Self::default();
        let Some(dir) = dir else { return q };
        let path = dir.join(FILE);
        if std::fs::symlink_metadata(&path).is_err() {
            return q;
        }
        // A queue is a few jobs: a file of more than 4 MB, or one that is no plain file or no
        // JSON, is damaged; put aside (not written over unseen) and said.
        let parsed = std::fs::metadata(&path)
            .ok()
            .filter(|m| m.is_file() && m.len() <= 4 << 20)
            .and_then(|_| lapsewerk_video::frames::read_text(&path).ok())
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        let Some(v) = parsed else {
            let aside = dir.join(format!(
                "queue (damaged {}).json",
                chrono::Local::now().format("%Y-%m-%d %H-%M-%S")
            ));
            if std::fs::rename(&path, &aside).is_ok() {
                q.damaged = Some(aside);
            }
            return q;
        };
        let text_of =
            |j: &serde_json::Value, k: &str| j.get(k).and_then(serde_json::Value::as_str).map(str::to_owned);
        let listed = v
            .get("jobs")
            .and_then(serde_json::Value::as_array)
            .map_or(0, |a| a.len().min(100));
        q.jobs = v
            .get("jobs")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|j| {
                        let args = j
                            .get("args")?
                            .as_array()?
                            .iter()
                            .map(|a| a.as_str().map(str::to_owned))
                            .collect::<Option<Vec<String>>>()?;
                        let state = text_of(j, "state")?;
                        // Sensible sizes only (a name, a command line).
                        if args.len() > 400 || args.iter().any(|a| a.len() > 8192) {
                            return None;
                        }
                        let name: String = text_of(j, "name")?.chars().take(200).collect();
                        Some(Job {
                            name,
                            folder: PathBuf::from(text_of(j, "folder")?),
                            out: PathBuf::from(text_of(j, "out")?),
                            args,
                            state: match state.as_str() {
                                "paused" => State::Paused,
                                "stopped" => State::Stopped,
                                s => State::Failed(s.strip_prefix("failed:").unwrap_or(s).to_owned()),
                            },
                        })
                    })
                    .take(100)
                    .collect()
            })
            .unwrap_or_default();
        // Some jobs unreadable: the file is copied aside before the queue is saved without them
        // (said as a damaged queue), not lost unseen.
        if q.jobs.len() < listed {
            let aside = dir.join(format!(
                "queue (damaged {}).json",
                chrono::Local::now().format("%Y-%m-%d %H-%M-%S")
            ));
            if std::fs::copy(&path, &aside).is_ok() {
                q.damaged = Some(aside);
            }
        }
        q.saved = serde_json::to_string_pretty(&q.kept()).unwrap_or_default();
        // Copied aside: the readable jobs are written back at once, so the next start finds a
        // sound file (no second copy, no second warning).
        if q.damaged.is_some() {
            q.saved.clear();
            q.save_at(Some(dir));
        }
        q
    }

    /// The export to `out` was finished outside the queue (step 6): a job there that stopped,
    /// failed or waits from last time is done too.
    pub fn finished_elsewhere(&mut self, out: &Path) {
        for j in &mut self.jobs {
            if j.out == out && matches!(j.state, State::Stopped | State::Failed(_) | State::Paused) {
                j.state = State::Done;
            }
        }
    }

    /// Lines up a paused job (from the last time the app ran) to run in its turn.
    pub fn resume_paused(&mut self, index: usize) {
        if let Some(j) = self.jobs.get_mut(index).filter(|j| j.state == State::Paused) {
            j.state = State::Waiting;
        }
    }

    /// Whether the queue's running job writes into `out` (its lock is ours, not another's).
    #[must_use]
    pub fn runs_target(&self, out: &Path) -> bool {
        self.jobs
            .iter()
            .any(|j| j.out == out && matches!(j.state, State::Running(..)))
    }

    /// Removes a job that is not running.
    pub fn remove(&mut self, index: usize) {
        if self.running.as_ref().is_some_and(|r| r.index == index) {
            return;
        }
        if index < self.jobs.len() {
            self.jobs.remove(index);
            if let Some(r) = self.running.as_mut()
                && r.index > index
            {
                r.index -= 1;
            }
        }
    }

    /// Runs a stopped or failed job again: going on with the frames there (`replace` false), or
    /// from the start, replacing what is there.
    pub fn requeue(&mut self, index: usize, replace: bool) {
        let Some(j) = self.jobs.get_mut(index) else {
            return;
        };
        if matches!(j.state, State::Waiting | State::Running(..)) {
            return;
        }
        j.args.retain(|a| a != "--resume" && a != "--overwrite");
        j.args
            .push(if replace { "--overwrite" } else { "--resume" }.into());
        j.state = State::Waiting;
    }

    /// Removes the jobs that are over (done, failed, stopped).
    pub fn clear_finished(&mut self) {
        let running = self.running.as_ref().map(|r| r.index);
        let mut kept = Vec::new();
        let mut new_running = None;
        for (i, j) in std::mem::take(&mut self.jobs).into_iter().enumerate() {
            if matches!(j.state, State::Waiting | State::Running(..) | State::Paused) {
                if running == Some(i) {
                    new_running = Some(kept.len());
                }
                kept.push(j);
            }
        }
        self.jobs = kept;
        if let (Some(r), Some(i)) = (self.running.as_mut(), new_running) {
            r.index = i;
        }
    }

    /// Stops the running job (its partial file is cleaned up by the command line).
    pub fn stop(&mut self) {
        if let Some(r) = self.running.as_mut() {
            r.stop_asked = true;
            terminate(&mut r.child);
        }
    }

    /// Moves the queue on: notes how the running job is doing, starts the next when it ends.
    /// Returns a line to tell the user when a job ended.
    pub fn poll(&mut self, cli: Option<&Path>) -> Option<(String, bool)> {
        let mut said = None;
        if let Some(r) = self.running.as_mut() {
            let ended = r.child.try_wait();
            // Ended: its last lines are read before they are looked at (waited for briefly: a
            // process it started may hold the output open, and the app must not hang on it).
            if matches!(ended, Ok(Some(_)) | Err(_))
                && let Some(h) = r.reader.take()
            {
                let until = std::time::Instant::now() + std::time::Duration::from_millis(500);
                while !h.is_finished() && std::time::Instant::now() < until {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                if h.is_finished() {
                    let _ = h.join();
                }
            }
            let shared = r.shared.lock().unwrap_or_else(PoisonError::into_inner);
            let (step, fraction, error) = (shared.step.clone(), shared.fraction, shared.error.clone());
            drop(shared);
            let index = r.index;
            match ended {
                Ok(None) => {
                    if let Some(j) = self.jobs.get_mut(index) {
                        j.state = State::Running(step, fraction);
                    }
                }
                Ok(Some(status)) => {
                    let stopped = r.stop_asked;
                    self.running = None;
                    if let Some(j) = self.jobs.get_mut(index) {
                        // Its folder held by another render (a terminal, another computer):
                        // waits and tries again in a minute, rather than failing.
                        let held = error.as_deref().is_some_and(|e| e.contains("another render"));
                        // Going on after the wait was refused (other settings, a kind of
                        // render that can't go on): its own arguments again, at once.
                        let refused = !stopped
                            && !status.success()
                            && error.as_deref().is_some_and(|e| {
                                e.contains("rendered with other settings")
                                    || e.contains("resuming works without")
                                    || e.contains("another file format")
                                    || e.contains("no light measurement")
                                    || e.contains("settings file is missing")
                            });
                        j.state = if stopped {
                            State::Stopped
                        } else if status.success() {
                            State::Done
                        } else if held {
                            State::Waiting
                        } else {
                            State::Failed(error.clone().unwrap_or_else(|| status.to_string()))
                        };
                        let own = self.resumed.take_if(|(i, _)| *i == index).map(|(_, a)| a);
                        if let Some(args) = own.filter(|_| refused) {
                            // What the other render left is not this one's: never replaced unasked.
                            // Said, with its reason; "Render anew (replaces)" replaces it.
                            j.args = args;
                            j.state = State::Failed(error.clone().unwrap_or_else(|| status.to_string()));
                            said = Some((j.name.clone(), false));
                        } else if held && !stopped && !status.success() {
                            // After the wait, the other render's frames are gone on with rather
                            // than all made again (the same render, finished there, is then
                            // only checked): `--resume` in place of `--overwrite`.
                            if let Some(at) = j.args.iter().position(|a| a == "--overwrite") {
                                self.resumed = Some((index, j.args.clone()));
                                if let Some(a) = j.args.get_mut(at) {
                                    "--resume".clone_into(a);
                                }
                            }
                            self.later = Some((
                                index,
                                std::time::Instant::now() + std::time::Duration::from_secs(60),
                            ));
                        } else {
                            said = Some((j.name.clone(), j.state == State::Done));
                        }
                    }
                }
                Err(e) => {
                    self.running = None;
                    if let Some(j) = self.jobs.get_mut(index) {
                        j.state = State::Failed(e.to_string());
                        said = Some((j.name.clone(), false));
                    }
                }
            }
        }
        let now = std::time::Instant::now();
        let waiting = |i: usize| !self.later.is_some_and(|(l, at)| l == i && now < at);
        if self.running.is_none()
            && let Some(index) = self
                .jobs
                .iter()
                .enumerate()
                .position(|(i, j)| j.state == State::Waiting && waiting(i))
        {
            if self.later.is_some_and(|(l, _)| l == index) {
                self.later = None;
            }
            self.start(index, cli);
        }
        said
    }

    fn start(&mut self, index: usize, cli: Option<&Path>) {
        let Some(job) = self.jobs.get_mut(index) else {
            return;
        };
        let Some(cli) = cli else {
            job.state = State::Failed("the command line (lapsewerk) is not beside the app".into());
            return;
        };
        let spawned = Command::new(cli)
            .arg("render")
            .arg(&job.folder)
            .args(&job.args)
            .env("LAPSEWERK_PROGRESS_LINES", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                job.state = State::Failed(e.to_string());
                return;
            }
        };
        let shared = Arc::new(Mutex::new(Shared::default()));
        let reader = child.stderr.take().map(|err| {
            let s = shared.clone();
            std::thread::spawn(move || read_progress(err, &s))
        });
        job.state = State::Running(String::new(), 0.0);
        self.running = Some(Running {
            index,
            child,
            shared,
            reader,
            stop_asked: false,
        });
    }
}

impl Queue {
    /// Whether job `index` waits for another render writing into its folder.
    #[must_use]
    pub fn held(&self, index: usize) -> bool {
        self.later.is_some_and(|(l, _)| l == index)
    }
}

impl Drop for Queue {
    /// Closing the app stops the running render (it would otherwise go on unseen).
    fn drop(&mut self) {
        if let Some(r) = self.running.as_mut() {
            terminate(&mut r.child);
            let _ = r.child.wait();
        }
    }
}

/// Reads the command line's error output: progress lines, and the last error.
fn read_progress(err: impl std::io::Read, shared: &Mutex<Shared>) {
    // Bounded lines: a runaway output cannot fill the memory.
    for line in BufReader::new(err.take(64 << 20)).lines() {
        let Ok(line) = line else { break };
        let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((step, done, total)) = parse_progress(&line) {
            if step != s.step || s.since.is_none() {
                s.since = Some(std::time::Instant::now());
            }
            s.step = step;
            s.fraction = if total > 0 {
                done as f32 / total as f32
            } else {
                0.0
            };
            s.counts = (done, total);
        } else if let Some(e) = line.strip_prefix("lapsewerk: ")
            && !e.starts_with("rendered ")
            && !e.starts_with("measured ")
            && !e.starts_with("checked ")
            && !e.starts_with("verified ")
        {
            s.error = Some(e.chars().take(300).collect());
        }
    }
}

/// Asks a render to stop so it can clean up (SIGTERM on Unix; killed elsewhere).
fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        let asked = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .is_ok_and(|s| s.success());
        if asked {
            return;
        }
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_are_read() {
        assert_eq!(
            parse_progress("progress rendered 30 120"),
            Some(("rendered".into(), 30, 120))
        );
        assert_eq!(
            parse_progress("progress measured 0 0"),
            Some(("measured".into(), 0, 0))
        );
        assert_eq!(parse_progress("lapsewerk: rendered 25/30"), None);
        assert_eq!(parse_progress("progress x"), None);
    }

    #[test]
    fn the_jobs_not_done_come_back_paused_after_a_restart() {
        let dir = std::env::temp_dir().join(format!("lw-queue-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut q = Queue::default();
        for (name, state) in [
            ("done", State::Done),
            ("running", State::Running("rendered".into(), 0.4)),
            ("waiting", State::Waiting),
            ("failed", State::Failed("no photos in x".into())),
        ] {
            q.add(
                name.into(),
                dir.join(name),
                dir.join(name).join("out"),
                vec!["--sequence".into()],
            );
            if let Some(j) = q.jobs.last_mut() {
                j.state = state;
            }
        }
        q.save_at(Some(&dir));
        let first = std::fs::metadata(dir.join(FILE)).unwrap().modified().unwrap();
        // Nothing changed (only how far the running one is): not written again.
        if let Some(j) = q.jobs.get_mut(1) {
            j.state = State::Running("rendered".into(), 0.6);
        }
        q.save_at(Some(&dir));
        assert_eq!(
            std::fs::metadata(dir.join(FILE)).unwrap().modified().unwrap(),
            first
        );
        let mut back = Queue::load_at(Some(&dir));
        let states: Vec<(&str, &State)> = back.jobs.iter().map(|j| (j.name.as_str(), &j.state)).collect();
        assert_eq!(
            states,
            [
                ("running", &State::Stopped),
                ("waiting", &State::Paused),
                ("failed", &State::Failed("no photos in x".into())),
            ]
        );
        assert_eq!(back.jobs[1].args, ["--sequence"]);
        // Nothing runs by itself; a paused job is not busy, but its target is taken.
        assert!(!back.busy());
        assert!(back.has_target(&dir.join("waiting").join("out")));
        back.clear_finished();
        assert_eq!(back.jobs.len(), 1, "a paused job is not finished");
        back.resume_paused(0);
        assert_eq!(back.jobs[0].state, State::Waiting);
        // Finished from step 6: the job stopped there is done too, and goes with "clear finished".
        back.jobs[0].state = State::Stopped;
        let out = back.jobs[0].out.clone();
        back.finished_elsewhere(&out);
        assert_eq!(back.jobs[0].state, State::Done);
        // A damaged file is an empty queue, not a crash, and is put aside, not written over.
        std::fs::write(dir.join(FILE), "{ not json").unwrap();
        let empty = Queue::load_at(Some(&dir));
        assert!(empty.jobs.is_empty());
        let aside = empty.damaged.clone().expect("put aside");
        assert_eq!(std::fs::read_to_string(&aside).unwrap(), "{ not json");
        assert!(!dir.join(FILE).exists());
        // One job unreadable among good ones: the good ones come back, the file is copied aside.
        let good = r#"{"name":"A","folder":"/a","out":"/a/out","args":["render"],"state":"paused"}"#;
        std::fs::write(dir.join(FILE), format!(r#"{{"jobs":[{good},{{"name":5}}]}}"#)).unwrap();
        let part = Queue::load_at(Some(&dir));
        assert_eq!(part.jobs.len(), 1);
        assert!(part.damaged.as_ref().is_some_and(|p| p.exists()), "copied aside");
        let again = Queue::load_at(Some(&dir));
        assert!(again.damaged.is_none(), "written back sound");
        assert_eq!(again.jobs.len(), 1);
        assert!(Queue::load_at(None).jobs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn jobs_run_one_after_another_and_report_how_they_ended() {
        // A stand-in for the command line: fails for a folder named "bad", else prints progress.
        let dir = std::env::temp_dir().join(format!("lw-queue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("lapsewerk");
        std::fs::write(
            &cli,
            "#!/bin/sh\necho 'progress rendered 1 2' >&2\nsleep 0.2\nif [ \"$2\" = bad ]; then echo 'lapsewerk: no photos in bad' >&2; exit 1; fi\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut q = Queue::default();
        q.add("A".into(), "good".into(), dir.join("a.mov"), vec![]);
        q.add("B".into(), "bad".into(), dir.join("b.mov"), vec![]);
        q.add("C".into(), "good".into(), dir.join("c.mov"), vec![]);
        assert!(q.has_target(&dir.join("b.mov")));
        let mut ended = Vec::new();
        for _ in 0..200 {
            if let Some(e) = q.poll(Some(&cli)) {
                ended.push(e);
            }
            // Never two at once.
            assert!(
                q.jobs
                    .iter()
                    .filter(|j| matches!(j.state, State::Running(..)))
                    .count()
                    <= 1
            );
            if !q.busy() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            ended,
            vec![("A".into(), true), ("B".into(), false), ("C".into(), true)]
        );
        assert_eq!(q.jobs[1].state, State::Failed("no photos in bad".into()));
        q.clear_finished();
        assert!(q.jobs.is_empty());
        // Without the command line: the job fails with a reason, nothing hangs.
        q.add("D".into(), "good".into(), dir.join("d.mov"), vec![]);
        q.poll(None);
        assert!(matches!(q.jobs[0].state, State::Failed(_)));
        // A long job, stopped.
        std::fs::write(&cli, "#!/bin/sh\nsleep 30\n").unwrap();
        q.clear_finished();
        q.add("E".into(), "good".into(), dir.join("e.mov"), vec![]);
        q.poll(Some(&cli));
        q.stop();
        let mut state = State::Waiting;
        for _ in 0..200 {
            q.poll(Some(&cli));
            state = q.jobs[0].state.clone();
            if !matches!(state, State::Running(..)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(state, State::Stopped);
        // Run again: going on with the frames there, or replacing them.
        q.jobs[0].args = vec!["--sequence".into(), "--overwrite".into()];
        q.requeue(0, false);
        assert_eq!(q.jobs[0].state, State::Waiting);
        assert_eq!(q.jobs[0].args, ["--sequence", "--resume"]);
        q.jobs[0].state = State::Failed("x".into());
        q.requeue(0, true);
        assert_eq!(q.jobs[0].args, ["--sequence", "--overwrite"]);
        q.jobs.clear();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

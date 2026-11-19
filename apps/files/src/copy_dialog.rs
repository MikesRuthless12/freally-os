// SPDX-License-Identifier: Apache-2.0
//! The copy dialog's model (TASK-105A, FR-085), the way Windows does it:
//! one window for every running copy, each with its progress, items and
//! bytes left, the time left, Pause and Cancel, and *More details* (a live
//! speed graph); the *Replace or Skip Files* choice when a name is taken,
//! and *Let me decide for each file* (both files' sizes and dates side by
//! side, a box for each to keep: both kept is *Keep both*); a failure
//! offers Try again, Skip and Cancel.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use app_session::import::size;
use fwire::proto::vfs as v;

/// A name taken: the file being copied and the one in its way, and which
/// to keep (in *Let me decide for each file*).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clash {
    pub src: String,
    pub dst: String,
    pub src_size: u64,
    pub src_modified_ns: u64,
    pub dst_size: u64,
    pub dst_modified_ns: u64,
    pub keep_src: bool,
    pub keep_dst: bool,
}

impl Clash {
    pub fn from_wire(c: &v::CopyConflict) -> Clash {
        Clash {
            src: c.src.clone(),
            dst: c.dst.clone(),
            src_size: c.src_size,
            src_modified_ns: c.src_modified_ns,
            dst_size: c.dst_size,
            dst_modified_ns: c.dst_modified_ns,
            keep_src: true,
            keep_dst: false,
        }
    }

    pub fn name(&self) -> &str {
        self.dst.rsplit('/').next().unwrap_or(&self.dst)
    }

    /// The choice its two boxes make: both kept is Keep both; only the new
    /// one, Replace; otherwise Skip (the one there stays).
    pub fn choice(&self) -> u8 {
        match (self.keep_src, self.keep_dst) {
            (true, true) => v::CHOOSE_KEEP_BOTH,
            (true, false) => v::CHOOSE_REPLACE,
            _ => v::CHOOSE_SKIP,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Running,
    Paused,
    /// Replace or Skip: this one.
    Conflict(Clash),
    /// Let me decide for each file: every one ahead.
    Deciding(Vec<Clash>),
    Failed(String),
    Done,
    Cancelled,
}

/// One running copy.
#[derive(Clone, Debug)]
pub struct Run {
    pub job: u32,
    pub moving: bool,
    pub title: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub done_items: u64,
    pub total_items: u64,
    pub current: String,
    pub phase: Phase,
    /// *More details* open.
    pub details: bool,
    /// *Do this for all conflicts*.
    pub all: bool,
    /// (when, bytes done) for the speed and the graph: the last minute.
    pub samples: VecDeque<(u64, u64)>,
}

/// A folder as the title names it: its last part, or *Home*.
fn place(path: &str, home: &str) -> String {
    if path == home {
        return String::from("Home");
    }
    String::from(path.rsplit('/').next().unwrap_or(path))
}

const MINUTE_NS: u64 = 60_000_000_000;

impl Run {
    pub fn new(job: &v::CopyJob, moving: bool, sources: &[String], dest: &str, home: &str, now: u64) -> Run {
        let from = sources.first().map_or(String::new(), |s| place(s.rsplit_once('/').map_or(s, |(d, _)| d), home));
        let n = sources.len();
        let what = if n == 1 {
            String::from(sources[0].rsplit('/').next().unwrap_or(""))
        } else {
            alloc::format!("{n} items")
        };
        let title =
            alloc::format!("{} {what} from {from} to {}", if moving { "Moving" } else { "Copying" }, place(dest, home));
        let mut samples = VecDeque::new();
        samples.push_back((now, 0));
        Run {
            job: job.job,
            moving,
            title,
            done_bytes: 0,
            total_bytes: job.total_bytes,
            done_items: 0,
            total_items: job.total_items,
            current: String::new(),
            phase: Phase::Running,
            details: false,
            all: false,
            samples,
        }
    }

    /// What vfs said after a step.
    pub fn update(&mut self, s: &v::CopyState, now: u64) {
        (self.done_bytes, self.total_bytes, self.done_items, self.total_items) =
            (s.done_bytes, s.total_bytes, s.done_items, s.total_items);
        self.current = s.current.clone();
        // A sample at most every 50 ms (File Manager's throttle).
        if self.samples.back().is_none_or(|&(t, _)| now >= t + 50_000_000) {
            self.samples.push_back((now, s.done_bytes));
            while self.samples.front().is_some_and(|&(t, _)| t + MINUTE_NS < now) {
                self.samples.pop_front();
            }
        }
        self.phase = match s.state {
            v::COPY_CONFLICT => Phase::Conflict(Clash::from_wire(&s.conflict)),
            v::COPY_FAILED => Phase::Failed(s.words.clone()),
            v::COPY_DONE => Phase::Done,
            v::COPY_CANCELLED => Phase::Cancelled,
            _ if self.phase == Phase::Paused => Phase::Paused,
            _ => Phase::Running,
        };
    }

    /// Bytes a second over the last three seconds of samples.
    pub fn speed(&self) -> f64 {
        let Some(&(t1, b1)) = self.samples.back() else { return 0.0 };
        let since = t1.saturating_sub(3_000_000_000);
        let Some(&(t0, b0)) = self.samples.iter().find(|&&(t, _)| t >= since) else { return 0.0 };
        if t1 <= t0 {
            return 0.0;
        }
        (b1 - b0) as f64 / ((t1 - t0) as f64 / 1e9)
    }

    /// The speed between each pair of samples, for the graph.
    pub fn speeds(&self) -> Vec<f64> {
        let s: Vec<&(u64, u64)> = self.samples.iter().collect();
        s.windows(2)
            .map(|w| {
                let dt = (w[1].0 - w[0].0) as f64 / 1e9;
                if dt > 0.0 { (w[1].1 - w[0].1) as f64 / dt } else { 0.0 }
            })
            .collect()
    }

    pub fn fraction(&self) -> f64 {
        if self.total_bytes == 0 {
            return if self.total_items == 0 { 1.0 } else { self.done_items as f64 / self.total_items as f64 };
        }
        (self.done_bytes as f64 / self.total_bytes as f64).min(1.0)
    }

    pub fn left_words(&self) -> String {
        let speed = self.speed();
        if self.samples.len() < 3 || speed < 1.0 {
            return String::from("Calculating…");
        }
        let secs = ((self.total_bytes - self.done_bytes.min(self.total_bytes)) as f64 / speed) as u64 + 1;
        match secs {
            0..60 => alloc::format!("About {secs} second{} left", if secs == 1 { "" } else { "s" }),
            60..3600 => {
                let m = secs.div_ceil(60);
                alloc::format!("About {m} minute{} left", if m == 1 { "" } else { "s" })
            }
            _ => {
                let h = secs.div_ceil(3600);
                alloc::format!("About {h} hour{} left", if h == 1 { "" } else { "s" })
            }
        }
    }

    /// "12.3 MB of 40.0 MB · 3 of 10 items · About 5 seconds left".
    pub fn words(&self) -> String {
        let left = match &self.phase {
            Phase::Paused => String::from("Paused"),
            Phase::Done => String::from("Done"),
            _ => self.left_words(),
        };
        alloc::format!(
            "{} of {} · {} of {} items · {left}",
            size(self.done_bytes),
            size(self.total_bytes),
            self.done_items,
            self.total_items
        )
    }

    pub fn speed_words(&self) -> String {
        alloc::format!("Speed: {}/s", size(self.speed() as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    fn job() -> v::CopyJob {
        v::CopyJob { job: 3, total_items: 10, total_bytes: 40_000_000 }
    }

    fn state(done_bytes: u64, done_items: u64, st: u8) -> v::CopyState {
        v::CopyState {
            state: st,
            done_items,
            total_items: 10,
            done_bytes,
            total_bytes: 40_000_000,
            current: String::from("/home/m/a.bin"),
            conflict: v::CopyConflict {
                src: String::from("/home/m/A/x.txt"),
                dst: String::from("/home/m/B/x.txt"),
                src_size: 5,
                src_modified_ns: 1,
                dst_size: 6,
                dst_modified_ns: 2,
            },
            words: String::from("/home/m/a.bin: the disk said no"),
        }
    }

    #[test]
    fn its_title_says_what_goes_where() {
        let srcs = [String::from("/home/m/A/x.txt"), String::from("/home/m/A/y.txt")];
        let r = Run::new(&job(), false, &srcs, "/home/m", "/home/m", 0);
        assert_eq!(r.title, "Copying 2 items from A to Home");
        let r = Run::new(&job(), true, &srcs[..1], "/home/m/B", "/home/m", 0);
        assert_eq!(r.title, "Moving x.txt from A to B");
    }

    #[test]
    fn speed_and_time_left_from_the_samples() {
        let mut r = Run::new(&job(), false, &[String::from("/home/m/a")], "/home/m/B", "/home/m", 0);
        assert_eq!(r.left_words(), "Calculating…");
        for i in 1..=4 {
            r.update(&state(i * 2_000_000, i, v::COPY_RUNNING), i * S);
        }
        assert!((r.speed() - 2_000_000.0).abs() < 1.0, "{}", r.speed());
        // 32 MB left at 2 MB/s.
        assert_eq!(r.left_words(), "About 17 seconds left");
        assert_eq!(r.words(), "8.0 MB of 40.0 MB · 4 of 10 items · About 17 seconds left");
        assert!((r.fraction() - 0.2).abs() < 1e-9);
        assert_eq!(r.speeds().len(), 4);
        r.phase = Phase::Paused;
        r.update(&state(8_000_000, 4, v::COPY_RUNNING), 5 * S);
        assert!(r.words().ends_with("Paused"), "paused stays paused");
    }

    #[test]
    fn a_minute_and_an_hour_left() {
        let mut r = Run::new(&job(), false, &[String::from("/home/m/a")], "/home/m/B", "/home/m", 0);
        r.total_bytes = 400_000_000_000;
        for i in 1..=3 {
            r.update(&state(i * 1_000_000, 0, v::COPY_RUNNING), i * S);
        }
        r.total_bytes = 200_000_000;
        assert_eq!(r.left_words(), "About 4 minutes left");
        r.total_bytes = 10_000_000_000;
        assert_eq!(r.left_words(), "About 3 hours left");
    }

    #[test]
    fn its_phase_follows_vfs() {
        let mut r = Run::new(&job(), false, &[String::from("/home/m/a")], "/home/m/B", "/home/m", 0);
        r.update(&state(0, 0, v::COPY_CONFLICT), S);
        let Phase::Conflict(c) = &r.phase else { panic!("no conflict") };
        assert_eq!((c.name(), c.src_size, c.dst_size), ("x.txt", 5, 6));
        r.update(&state(0, 0, v::COPY_FAILED), 2 * S);
        assert_eq!(r.phase, Phase::Failed(String::from("/home/m/a.bin: the disk said no")));
        r.update(&state(40_000_000, 10, v::COPY_DONE), 3 * S);
        assert_eq!(r.phase, Phase::Done);
    }

    #[test]
    fn the_two_boxes_make_the_choice() {
        let mut c = Clash::from_wire(&state(0, 0, 1).conflict);
        assert_eq!(c.choice(), v::CHOOSE_REPLACE, "the new one, by default");
        c.keep_dst = true;
        assert_eq!(c.choice(), v::CHOOSE_KEEP_BOTH);
        c.keep_src = false;
        assert_eq!(c.choice(), v::CHOOSE_SKIP);
        c.keep_dst = false;
        assert_eq!(c.choice(), v::CHOOSE_SKIP, "neither: the one there stays");
    }
}

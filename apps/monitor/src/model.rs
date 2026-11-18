// SPDX-License-Identifier: Apache-2.0
//! Monitor's data (TASK-062, FR-034, PRD § 8 *Monitor*), pure and
//! host-tested; `main.rs` fetches it and draws it.
//!
//! - **Processes:** every service with its description, state, memory, CPU %
//!   (from two samples a second apart; "—" until there are two) and
//!   restarts; sorted by any column, either way.
//! - **Boot:** each stage's time and its share of the boot, the firmware's
//!   own times apart from ours.
//! - **Faults:** newest first; one expands to its details.
//! - **Log:** filtered by service and level, kept to the last 2,000 lines.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    pub name: String,
    pub description: String,
    pub state: String,
    pub pid: u32,
    pub restarts: u32,
    pub mem_bytes: u64,
    /// CPU time so far, ns.
    pub cpu_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Name,
    State,
    Memory,
    Cpu,
    Restarts,
}

/// One row as shown: the process and its CPU % (None until a second sample).
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub proc: Proc,
    pub cpu_pct: Option<f32>,
}

#[derive(Debug)]
pub struct Processes {
    last: Vec<Proc>,
    last_ns: u64,
    pub rows: Vec<Row>,
    pub sort: Column,
    pub descending: bool,
}

impl Default for Processes {
    fn default() -> Processes {
        Processes { last: Vec::new(), last_ns: 0, rows: Vec::new(), sort: Column::Cpu, descending: true }
    }
}

impl Processes {
    /// A new sample taken at `now_ns`.
    pub fn sample(&mut self, procs: Vec<Proc>, now_ns: u64) {
        let dt = now_ns.saturating_sub(self.last_ns);
        self.rows = procs
            .iter()
            .map(|p| {
                let before = self.last.iter().find(|q| q.name == p.name && q.pid == p.pid);
                let cpu_pct = before
                    .filter(|_| dt > 0 && self.last_ns != 0)
                    .map(|q| (p.cpu_ns.saturating_sub(q.cpu_ns) as f64 * 100.0 / dt as f64).min(100.0) as f32);
                Row { proc: p.clone(), cpu_pct }
            })
            .collect();
        self.last = procs;
        self.last_ns = now_ns;
        self.order();
    }

    /// Clicking a column's header: sort by it, or turn the order around.
    pub fn sort_by(&mut self, c: Column) {
        if self.sort == c {
            self.descending = !self.descending;
        } else {
            self.sort = c;
            self.descending = matches!(c, Column::Memory | Column::Cpu | Column::Restarts);
        }
        self.order();
    }

    fn order(&mut self) {
        let (c, desc) = (self.sort, self.descending);
        self.rows.sort_by(|a, b| {
            let o = match c {
                Column::Name => a.proc.name.cmp(&b.proc.name),
                Column::State => a.proc.state.cmp(&b.proc.state),
                Column::Memory => a.proc.mem_bytes.cmp(&b.proc.mem_bytes),
                Column::Cpu => a.cpu_pct.unwrap_or(-1.0).total_cmp(&b.cpu_pct.unwrap_or(-1.0)),
                Column::Restarts => a.proc.restarts.cmp(&b.proc.restarts),
            };
            let o = if desc { o.reverse() } else { o };
            o.then_with(|| a.proc.name.cmp(&b.proc.name))
        });
    }
}

/// "12.3 MiB", "640 KiB".
pub fn bytes(b: u64) -> String {
    if b >= 1 << 20 {
        alloc::format!("{:.1} MiB", b as f64 / (1u64 << 20) as f64)
    } else {
        alloc::format!("{} KiB", b.div_ceil(1024))
    }
}

/// "3.2 %", or "—" before the second sample.
pub fn cpu(p: Option<f32>) -> String {
    p.map_or(String::from("—"), |v| alloc::format!("{v:.1} %"))
}

/// The boot, stage by stage.
#[derive(Clone, Debug, PartialEq)]
pub struct Boot {
    /// Our stages: name, ms from B0, and the span before it (ms).
    pub stages: Vec<(String, f64, f64)>,
    pub total_ms: f64,
    /// The firmware's own times (F0 reset end, F1 loader start), when the FPDT has them.
    pub firmware: Option<(f64, f64)>,
}

impl Boot {
    /// From (stage, ms) pairs as conductor's timeline gives them.
    pub fn of(stages: &[(String, f64)]) -> Boot {
        let f = |n: &str| stages.iter().find(|(s, _)| s == n).map(|(_, ms)| *ms);
        let firmware = f("F0").zip(f("F1"));
        let mut ours: Vec<(String, f64, f64)> = Vec::new();
        let mut last = 0.0;
        for (name, ms) in stages.iter().filter(|(n, _)| !n.starts_with('F')) {
            ours.push((name.clone(), *ms, (ms - last).max(0.0)));
            last = *ms;
        }
        let total_ms = ours.last().map_or(0.0, |s| s.1);
        Boot { stages: ours, total_ms, firmware }
    }

    /// A stage's span as a share of the whole boot (0..=1): its meter.
    pub fn share(&self, i: usize) -> f32 {
        if self.total_ms <= 0.0 { 0.0 } else { (self.stages[i].2 / self.total_ms) as f32 }
    }

    /// "Firmware 4.1 s · Freally OS 1.2 s" when the firmware's times are known.
    pub fn summary(&self) -> String {
        let secs = |ms: f64| alloc::format!("{:.1} s", ms / 1000.0);
        match self.firmware {
            Some((_, f1)) => alloc::format!("Firmware {} · Freally OS {}", secs(f1), secs(self.total_ms)),
            None => alloc::format!("Freally OS {} (this firmware doesn't report its own time)", secs(self.total_ms)),
        }
    }
}

/// The log, as Monitor keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub ms: f64,
    pub service: String,
    pub level: u8,
    pub msg: String,
}

pub const LOG_KEEP: usize = 2000;

#[derive(Debug, Default)]
pub struct Log {
    pub lines: Vec<Line>,
    /// Only this service, if any.
    pub service: Option<String>,
    /// Only this level and above.
    pub min_level: u8,
}

impl Log {
    pub fn push(&mut self, l: Line) {
        if self.lines.len() == LOG_KEEP {
            self.lines.remove(0);
        }
        self.lines.push(l);
    }

    /// The lines the filter lets through, oldest first.
    pub fn shown(&self) -> Vec<&Line> {
        self.lines
            .iter()
            .filter(|l| l.level >= self.min_level && self.service.as_ref().is_none_or(|s| *s == l.service))
            .collect()
    }

    /// The services seen, for the filter's choices.
    pub fn services(&self) -> Vec<String> {
        let mut s: Vec<String> = self.lines.iter().map(|l| l.service.clone()).collect();
        s.sort();
        s.dedup();
        s
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaultRow {
    pub seq: u64,
    pub service: String,
    pub kind: String,
    pub message: String,
    pub rip: u64,
    pub addr: u64,
    pub frames: Vec<u64>,
}

/// What a fault row says expanded.
pub fn fault_details(f: &FaultRow) -> Vec<String> {
    let mut out = alloc::vec![
        alloc::format!("{} in {} (#{})", f.kind, f.service, f.seq),
        f.message.clone(),
        alloc::format!("at {:#x}, touching {:#x}", f.rip, f.addr),
    ];
    for (i, a) in f.frames.iter().enumerate() {
        out.push(alloc::format!("  {i:>2}  {a:#018x}"));
    }
    out
}

#[cfg(test)]
mod tests;

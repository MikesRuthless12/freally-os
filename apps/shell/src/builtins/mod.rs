// SPDX-License-Identifier: Apache-2.0
//! The built-ins (TASK-061, FR-033): `help`, `ps`, `boot`, `log [service]
//! [-f]`, `mem`, `uptime`, `clear`, `theme dark|light`, `scale <pct>`,
//! `reboot [other]`, `faults`, `version`, and `restart <service>` in dev
//! images. Each answers `--help`. Parsing and the words they print are
//! pure; what they reach (conductor, the logger, stage) is behind
//! `System`, so every one is tested on the host with a pretend system.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub description: String,
    pub state: String,
    pub pid: u32,
    pub restarts: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub ms: f64,
    pub service: String,
    pub level: u8,
    pub msg: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub seq: u64,
    pub service: String,
    pub kind: String,
    pub message: String,
}

/// What the built-ins reach.
pub trait System {
    fn services(&mut self) -> Result<Vec<Service>, String>;
    /// Each boot stage and its time in ms from B0 (F0 and F1 are the firmware's own).
    fn boot(&mut self) -> Result<Vec<(String, f64)>, String>;
    /// Log records after `cursor` (0: from the oldest kept), at most `limit`;
    /// and the cursor to ask from next.
    fn log(&mut self, service: Option<&str>, cursor: u64, limit: u16) -> Result<(Vec<Record>, u64), String>;
    /// Memory: (total, used) bytes.
    fn mem(&mut self) -> Result<(u64, u64), String>;
    fn uptime_ns(&mut self) -> u64;
    fn theme(&mut self, dark: bool) -> Result<(), String>;
    fn scale(&mut self, pct: u16) -> Result<(), String>;
    fn reboot(&mut self, other: bool) -> Result<(), String>;
    fn faults(&mut self) -> Result<Vec<Fault>, String>;
    fn version(&self) -> String;
    fn restart(&mut self, service: &str) -> Result<(), String>;
    /// A dev image: `restart` is there.
    fn dev(&self) -> bool;
    // Files (TASK-100), through vfs: an `Err` holds vfs's words.
    /// A directory's entries: each name and whether it's a directory.
    fn list(&mut self, path: &str) -> Result<Vec<(String, bool)>, String>;
    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, String>;
    fn write_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), String>;
    fn make_dir(&mut self, path: &str) -> Result<(), String>;
    fn remove(&mut self, path: &str) -> Result<(), String>;
    fn rename(&mut self, from: &str, to: &str) -> Result<(), String>;
    /// The session's account (empty: none).
    fn whoami(&mut self) -> Result<String, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help(Option<String>),
    Ps,
    Boot,
    Log { service: Option<String>, follow: bool },
    Mem,
    Uptime,
    Clear,
    Theme { dark: bool },
    Scale { pct: u16 },
    Reboot { other: bool },
    Faults,
    Version,
    Restart { service: String },
    Ls { path: String },
    Cat { path: String },
    Write { path: String, text: String },
    Mkdir { path: String },
    Rm { path: String },
    Mv { from: String, to: String },
    Whoami,
}

/// Every built-in, its usage and what it does.
pub const BUILTINS: &[(&str, &str, &str)] = &[
    ("help", "help [command]", "Lists the commands, or says what one does."),
    ("ps", "ps", "Lists every service: its state, process, restarts and what it does."),
    ("boot", "boot", "Shows how long each stage of this boot took, from the bootloader to the desktop."),
    ("log", "log [service] [-f]", "Shows the log, or one service's; -f keeps showing new lines until Ctrl+C."),
    ("mem", "mem", "Shows how much memory is in use."),
    ("uptime", "uptime", "Shows how long Freally OS has been running."),
    ("clear", "clear", "Clears the screen."),
    ("theme", "theme dark|light", "Switches every window to the dark or the light theme."),
    ("scale", "scale <pct>", "Sets the UI scale, 100 to 300 in steps of 25."),
    ("reboot", "reboot [other]", "Restarts the computer; with other, from the other system slot."),
    ("faults", "faults", "Lists what has crashed since Freally OS started."),
    ("version", "version", "Shows the version of Freally OS."),
    ("restart", "restart <service>", "Restarts a service (dev images only)."),
    ("ls", "ls [path]", "Lists a folder (/ when none is given). Folders end with /."),
    ("cat", "cat <path>", "Shows what a file holds."),
    ("write", "write <path> <text>", "Writes the text (and a new line) into a file, made or replaced."),
    ("mkdir", "mkdir <path>", "Makes a folder."),
    ("rm", "rm <path>", "Removes a file, or an empty folder."),
    ("mv", "mv <from> <to>", "Moves or renames a file or folder."),
    ("whoami", "whoami", "Shows which account this session belongs to."),
];

/// The words for `help` or `help <command>`.
pub fn help(what: Option<&str>, dev: bool) -> String {
    match what {
        Some(name) => match BUILTINS.iter().find(|b| b.0 == name && (dev || name != "restart")) {
            Some((_, usage, about)) => format!("{usage}\n  {about}\n"),
            None => format!("help: there's no command called '{name}'. Type help to see them.\n"),
        },
        None => {
            let mut s = String::from("Commands (add --help to any of them):\n");
            for (name, usage, about) in BUILTINS {
                if *name == "restart" && !dev {
                    continue;
                }
                s.push_str(&format!("  {usage:<20} {about}\n"));
            }
            s
        }
    }
}

/// A line to a command. `Ok(None)` for a blank line; `Err` holds the words
/// for a mistake (with the command's usage).
pub fn parse(line: &str, dev: bool) -> Result<Option<Command>, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let Some((&name, args)) = words.split_first() else { return Ok(None) };
    if args.iter().any(|a| *a == "--help" || *a == "-h") {
        return Ok(Some(Command::Help(Some(name.to_string()))));
    }
    let usage = |name: &str| BUILTINS.iter().find(|b| b.0 == name).map_or(String::new(), |b| format!("usage: {}", b.1));
    let none = |c: Command| {
        if args.is_empty() { Ok(Some(c)) } else { Err(format!("{name} takes nothing more. {}", usage(name))) }
    };
    match name {
        "help" => match args {
            [] => Ok(Some(Command::Help(None))),
            [what] => Ok(Some(Command::Help(Some(what.to_string())))),
            _ => Err(usage("help")),
        },
        "ps" => none(Command::Ps),
        "boot" => none(Command::Boot),
        "mem" => none(Command::Mem),
        "uptime" => none(Command::Uptime),
        "clear" => none(Command::Clear),
        "faults" => none(Command::Faults),
        "version" => none(Command::Version),
        "log" => {
            let follow = args.contains(&"-f");
            let rest: Vec<&&str> = args.iter().filter(|a| **a != "-f").collect();
            match rest.as_slice() {
                [] => Ok(Some(Command::Log { service: None, follow })),
                [s] if !s.starts_with('-') => Ok(Some(Command::Log { service: Some(s.to_string()), follow })),
                _ => Err(usage("log")),
            }
        }
        "theme" => match args {
            ["dark"] => Ok(Some(Command::Theme { dark: true })),
            ["light"] => Ok(Some(Command::Theme { dark: false })),
            _ => Err(usage("theme")),
        },
        "scale" => match args {
            [pct] => match pct.trim_end_matches('%').parse::<u16>() {
                Ok(p) if (100..=300).contains(&p) && p % 25 == 0 => Ok(Some(Command::Scale { pct: p })),
                _ => Err(format!("scale: {pct} isn't 100 to 300 in steps of 25. {}", usage("scale"))),
            },
            _ => Err(usage("scale")),
        },
        "reboot" => match args {
            [] => Ok(Some(Command::Reboot { other: false })),
            ["other"] => Ok(Some(Command::Reboot { other: true })),
            _ => Err(usage("reboot")),
        },
        "restart" if dev => match args {
            [s] => Ok(Some(Command::Restart { service: s.to_string() })),
            _ => Err(usage("restart")),
        },
        "whoami" => none(Command::Whoami),
        "ls" | "cat" | "mkdir" | "rm" => {
            let path = match (name, args) {
                ("ls", []) => "/",
                (_, [p]) => p,
                _ => return Err(usage(name)),
            };
            if !path.starts_with('/') {
                return Err(format!("{name}: paths start with / (/home/<you>/...). {}", usage(name)));
            }
            let path = path.to_string();
            Ok(Some(match name {
                "ls" => Command::Ls { path },
                "cat" => Command::Cat { path },
                "mkdir" => Command::Mkdir { path },
                _ => Command::Rm { path },
            }))
        }
        "write" => match args {
            [p, rest @ ..] if p.starts_with('/') && !rest.is_empty() => {
                Ok(Some(Command::Write { path: p.to_string(), text: rest.join(" ") }))
            }
            _ => Err(usage("write")),
        },
        "mv" => match args {
            [a, b] if a.starts_with('/') && b.starts_with('/') => {
                Ok(Some(Command::Mv { from: a.to_string(), to: b.to_string() }))
            }
            _ => Err(usage("mv")),
        },
        _ => Err(format!("{name}: there's no such command. Type help to see them.")),
    }
}

/// What a command gives back: its words, whether to clear the screen
/// first, and (for `log -f`) where the log stops, to follow from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    pub text: String,
    pub clear: bool,
    pub follow: Option<Follow>,
}

/// `log -f` going on: its service, and the cursor to ask from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Follow {
    pub service: Option<String>,
    pub cursor: u64,
}

fn mib(b: u64) -> String {
    format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))
}

/// One record as `log` shows it.
pub fn record(r: &Record) -> String {
    let level = ["trace", "debug", "info", "warn", "error"].get(r.level as usize).copied().unwrap_or("?");
    format!("{:>10.3} {:<12} {:<5} {}\n", r.ms / 1000.0, r.service, level, r.msg)
}

/// Runs `cmd` against `sys`.
pub fn run(cmd: &Command, sys: &mut impl System) -> Output {
    let text = |s: String| Output { text: s, ..Output::default() };
    let fail = |e: String| Output { text: format!("{e}\n"), ..Output::default() };
    match cmd {
        Command::Help(what) => text(help(what.as_deref(), sys.dev())),
        Command::Clear => Output { clear: true, ..Output::default() },
        Command::Version => text(format!("{}\n", sys.version())),
        Command::Uptime => {
            let s = sys.uptime_ns() / 1_000_000_000;
            let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
            text(if h > 0 { format!("up {h} h {m} min\n") } else { format!("up {m} min {s} s\n") })
        }
        Command::Mem => match sys.mem() {
            Ok((total, used)) => {
                text(format!("{} used of {} ({}%)\n", mib(used), mib(total), used * 100 / total.max(1)))
            }
            Err(e) => fail(e),
        },
        Command::Ps => match sys.services() {
            Ok(list) => {
                let mut s =
                    format!("{:<16} {:<10} {:>5} {:>8}  {}\n", "SERVICE", "STATE", "PID", "RESTARTS", "WHAT IT DOES");
                for v in list {
                    let pid = if v.pid == 0 { String::from("-") } else { v.pid.to_string() };
                    s.push_str(&format!(
                        "{:<16} {:<10} {:>5} {:>8}  {}\n",
                        v.name, v.state, pid, v.restarts, v.description
                    ));
                }
                text(s)
            }
            Err(e) => fail(e),
        },
        Command::Boot => match sys.boot() {
            Ok(stages) => {
                let mut s = String::new();
                let (firmware, ours): (Vec<_>, Vec<_>) = stages.iter().partition(|(n, _)| n.starts_with('F'));
                let mut last = 0.0;
                for (name, ms) in &ours {
                    s.push_str(&format!("{name:<3} {ms:>9.1} ms  (+{:.1})\n", ms - last));
                    last = *ms;
                }
                if let Some((n, ms)) = ours.last() {
                    s.push_str(&format!("B0 to {n}: {ms:.1} ms\n"));
                }
                if firmware.len() == 2 {
                    s.push_str(&format!(
                        "The firmware's own: F0 {:.1} ms, F1 {:.1} ms\n",
                        firmware[0].1, firmware[1].1
                    ));
                }
                text(s)
            }
            Err(e) => fail(e),
        },
        Command::Faults => match sys.faults() {
            Ok(list) if list.is_empty() => text(String::from("Nothing has crashed since Freally OS started.\n")),
            Ok(list) => {
                let mut s = String::new();
                for f in list {
                    s.push_str(&format!("#{} {} ({}): {}\n", f.seq, f.service, f.kind, f.message));
                }
                text(s)
            }
            Err(e) => fail(e),
        },
        Command::Log { service, follow } => match sys.log(service.as_deref(), 0, 200) {
            Ok((records, cursor)) => {
                let mut s: String = records.iter().map(record).collect();
                if records.is_empty() {
                    s = String::from("No log lines yet.\n");
                }
                Output { text: s, clear: false, follow: follow.then(|| Follow { service: service.clone(), cursor }) }
            }
            Err(e) => fail(e),
        },
        Command::Theme { dark } => match sys.theme(*dark) {
            Ok(()) => text(String::new()),
            Err(e) => fail(e),
        },
        Command::Scale { pct } => match sys.scale(*pct) {
            Ok(()) => text(String::new()),
            Err(e) => fail(e),
        },
        Command::Reboot { other } => match sys.reboot(*other) {
            Ok(()) => text(String::from("Restarting…\n")),
            Err(e) => fail(e),
        },
        Command::Restart { service } => match sys.restart(service) {
            Ok(()) => text(format!("{service} restarted.\n")),
            Err(e) => fail(e),
        },
        Command::Ls { path } => match sys.list(path) {
            Ok(list) if list.is_empty() => text(String::from("(empty)\n")),
            Ok(list) => {
                text(list.into_iter().map(|(n, dir)| if dir { format!("{n}/\n") } else { format!("{n}\n") }).collect())
            }
            Err(e) => fail(format!("ls: {path}: {e}")),
        },
        Command::Cat { path } => match sys.read_file(path) {
            Ok(bytes) => text(String::from_utf8_lossy(&bytes).into_owned()),
            Err(e) => fail(format!("cat: {path}: {e}")),
        },
        Command::Write { path, text: t } => match sys.write_file(path, format!("{t}\n").as_bytes()) {
            Ok(()) => text(String::new()),
            Err(e) => fail(format!("write: {path}: {e}")),
        },
        Command::Mkdir { path } => match sys.make_dir(path) {
            Ok(()) => text(String::new()),
            Err(e) => fail(format!("mkdir: {path}: {e}")),
        },
        Command::Rm { path } => match sys.remove(path) {
            Ok(()) => text(String::new()),
            Err(e) => fail(format!("rm: {path}: {e}")),
        },
        Command::Mv { from, to } => match sys.rename(from, to) {
            Ok(()) => text(String::new()),
            Err(e) => fail(format!("mv: {from}: {e}")),
        },
        Command::Whoami => match sys.whoami() {
            Ok(u) if u.is_empty() => text(String::from("No one: no session (log in first).\n")),
            Ok(u) => text(format!("{u}\n")),
            Err(e) => fail(format!("whoami: {e}")),
        },
    }
}

#[cfg(test)]
mod tests;

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

/// A network interface, as netstack says it (TASK-174).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    pub driver: String,
    pub mac: [u8; 6],
    pub link: bool,
    pub ipv4: Option<([u8; 4], u8)>,
    pub router: Option<[u8; 4]>,
    pub dns: Vec<[u8; 4]>,
    pub lease_s: u32,
    pub ipv6: Vec<([u8; 16], u8)>,
    pub router6: Option<[u8; 16]>,
    pub dns6: Vec<[u8; 16]>,
}

/// Where a ping goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    V4([u8; 4]),
    V6([u8; 16]),
}

impl Target {
    fn words(&self) -> String {
        match self {
            Target::V4(a) => dotted(*a),
            Target::V6(a) => fnet::ipv6::text(a),
        }
    }
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
    /// A file of `bytes` bytes (a pattern, not zeros).
    fn make_file(&mut self, path: &str, bytes: u64) -> Result<(), String>;
    /// A copy through the engine (TASK-105A), names taken replaced: (items,
    /// bytes, ns it took).
    fn copy(&mut self, sources: &[String], dest: &str, verify: bool) -> Result<(u64, u64, u64), String>;
    /// The network's interfaces (TASK-174).
    fn interfaces(&mut self) -> Result<Vec<Interface>, String>;
    /// One echo request to `to`: its round trip in µs.
    fn ping(&mut self, to: Target, seq: u16) -> Result<u32, String>;
}

/// "10.0.2.15".
fn dotted(a: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

/// "10.0.2.15" → its four bytes.
pub fn ipv4_of(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = s.split('.');
    for b in out.iter_mut() {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        *b = p.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// `ip`'s words for one interface.
fn interface_words(i: &Interface) -> String {
    let m = i.mac;
    let mut s = format!(
        "{}  {}  {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  link {}\n",
        i.name,
        i.driver,
        m[0],
        m[1],
        m[2],
        m[3],
        m[4],
        m[5],
        if i.link { "up" } else { "down" }
    );
    match i.ipv4 {
        Some((a, prefix)) => {
            s.push_str(&format!("  inet {}/{prefix}", dotted(a)));
            if let Some(r) = i.router {
                s.push_str(&format!("  router {}", dotted(r)));
            }
            if !i.dns.is_empty() {
                let dns: Vec<String> = i.dns.iter().map(|d| dotted(*d)).collect();
                s.push_str(&format!("  dns {}", dns.join(" ")));
            }
            s.push_str(&format!("  lease {} s\n", i.lease_s));
        }
        None => s.push_str("  no address yet (asking DHCP)\n"),
    }
    let last = i.ipv6.len().saturating_sub(1);
    for (n, (a, p)) in i.ipv6.iter().enumerate() {
        s.push_str(&format!("  inet6 {}/{p}", fnet::ipv6::text(a)));
        if n == last {
            if let Some(r) = i.router6 {
                s.push_str(&format!("  router {}", fnet::ipv6::text(&r)));
            }
            if !i.dns6.is_empty() {
                let dns: Vec<String> = i.dns6.iter().map(fnet::ipv6::text).collect();
                s.push_str(&format!("  dns {}", dns.join(" ")));
            }
        }
        s.push('\n');
    }
    s
}

/// "64M" → 67,108,864 (K, M, G are powers of 1,024); at most 1 GiB.
pub fn size_of(s: &str) -> Option<u64> {
    let (digits, unit) = match s.char_indices().last()? {
        (i, c) if c.is_ascii_alphabetic() => (&s[..i], c.to_ascii_uppercase()),
        _ => (s, ' '),
    };
    let n: u64 = digits.parse().ok()?;
    let n = match unit {
        ' ' => n,
        'K' => n.checked_mul(1 << 10)?,
        'M' => n.checked_mul(1 << 20)?,
        'G' => n.checked_mul(1 << 30)?,
        _ => return None,
    };
    (n <= 1 << 30).then_some(n)
}

/// "64.0 MiB", "512 bytes".
fn bytes_words(b: u64) -> String {
    match b {
        0..1024 => format!("{b} bytes"),
        1024..1_048_576 => format!("{:.1} KiB", b as f64 / 1024.0),
        _ => format!("{:.1} MiB", b as f64 / 1_048_576.0),
    }
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
    Mkfile { path: String, bytes: u64 },
    Cp { sources: Vec<String>, dest: String, verify: bool },
    Ip,
    Ping { to: Target, count: u16 },
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
    ("mkfile", "mkfile <path> <size>", "Makes a file of that size (512, 64K, 8M, at most 1G), to try copies with."),
    ("cp", "cp [--verify] <from>... <folder>", "Copies into a folder through the copy engine, and says how fast."),
    ("ip", "ip", "Shows each network interface: its driver, MAC, link, address, router and DNS."),
    (
        "ping",
        "ping <address> [count]",
        "Sends echo requests (3, or count up to 10), each after the last's answer, and shows each round trip.",
    ),
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
        "ip" => none(Command::Ip),
        "ping" => {
            let to = |a: &str| {
                ipv4_of(a)
                    .map(Target::V4)
                    .or_else(|| fnet::ipv6::parse_text(a).map(Target::V6))
                    .ok_or(format!("ping: {a} isn't an IPv4 or IPv6 address. {}", usage("ping")))
            };
            match args {
                [a] => Ok(Some(Command::Ping { to: to(a)?, count: 3 })),
                [a, n] => match n.parse::<u16>() {
                    Ok(c) if (1..=10).contains(&c) => Ok(Some(Command::Ping { to: to(a)?, count: c })),
                    _ => Err(format!("ping: the count is 1 to 10. {}", usage("ping"))),
                },
                _ => Err(usage("ping")),
            }
        }
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
        "mkfile" => match args {
            [p, n] if p.starts_with('/') => match size_of(n) {
                Some(bytes) => Ok(Some(Command::Mkfile { path: p.to_string(), bytes })),
                None => Err(format!("mkfile: {n} isn't a size (512, 64K, 8M, at most 1G).")),
            },
            _ => Err(usage("mkfile")),
        },
        "cp" => {
            let verify = args.first() == Some(&"--verify");
            let paths = if verify { &args[1..] } else { args };
            match paths {
                [from @ .., dest] if !from.is_empty() && paths.iter().all(|p| p.starts_with('/')) => {
                    Ok(Some(Command::Cp {
                        sources: from.iter().map(|s| s.to_string()).collect(),
                        dest: dest.to_string(),
                        verify,
                    }))
                }
                _ => Err(usage("cp")),
            }
        }
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
                // One line a service in the Terminal's 96 columns: 44 for the
                // rest, so a description longer than 52 is cut short.
                const ROOM: usize = 52;
                for v in list {
                    let pid = if v.pid == 0 { String::from("-") } else { v.pid.to_string() };
                    let what: String = if v.description.chars().count() > ROOM {
                        v.description.chars().take(ROOM - 1).chain(core::iter::once('…')).collect()
                    } else {
                        v.description
                    };
                    s.push_str(&format!("{:<16} {:<10} {:>5} {:>8}  {what}\n", v.name, v.state, pid, v.restarts));
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
        Command::Mkfile { path, bytes } => match sys.make_file(path, *bytes) {
            Ok(()) => text(String::new()),
            Err(e) => fail(format!("mkfile: {path}: {e}")),
        },
        Command::Cp { sources, dest, verify } => match sys.copy(sources, dest, *verify) {
            Ok((items, bytes, ns)) => {
                let secs = ns as f64 / 1e9;
                let speed = if secs > 0.0 { bytes as f64 / secs } else { 0.0 };
                text(format!(
                    "Copied {items} item(s), {} in {secs:.2} s ({}/s){}.\n",
                    bytes_words(bytes),
                    bytes_words(speed as u64),
                    if *verify { ", each read back" } else { "" }
                ))
            }
            Err(e) => fail(format!("cp: {e}")),
        },
        Command::Whoami => match sys.whoami() {
            Ok(u) if u.is_empty() => text(String::from("No one: no session (log in first).\n")),
            Ok(u) => text(format!("{u}\n")),
            Err(e) => fail(format!("whoami: {e}")),
        },
        Command::Ip => match sys.interfaces() {
            Ok(list) if list.is_empty() => text(String::from("No network interfaces.\n")),
            Ok(list) => text(list.iter().map(interface_words).collect()),
            Err(e) => fail(format!("ip: {e}")),
        },
        Command::Ping { to, count } => {
            let mut s = String::new();
            let mut answered = 0;
            for seq in 1..=*count {
                match sys.ping(*to, seq) {
                    Ok(us) => {
                        answered += 1;
                        s.push_str(&format!(
                            "reply from {}: seq={seq} time={:.2} ms\n",
                            to.words(),
                            us as f64 / 1000.0
                        ));
                    }
                    Err(e) => s.push_str(&format!("no reply from {}: seq={seq} ({e})\n", to.words())),
                }
            }
            s.push_str(&format!("{count} sent, {answered} answered.\n"));
            text(s)
        }
    }
}

#[cfg(test)]
mod tests;

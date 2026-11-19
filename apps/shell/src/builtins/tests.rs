// SPDX-License-Identifier: Apache-2.0
//! Every built-in's parser, and what each prints, against a pretend system.

use super::*;
use alloc::vec;

#[derive(Default)]
struct Pretend {
    theme: Option<bool>,
    scale: Option<u16>,
    restarted: Vec<String>,
    dev: bool,
    /// Files by path (folders hold None).
    files: alloc::collections::BTreeMap<String, Option<Vec<u8>>>,
}

impl System for Pretend {
    fn services(&mut self) -> Result<Vec<Service>, String> {
        Ok(vec![
            Service {
                name: "logger".into(),
                description: "Keeps the log.".into(),
                state: "Running".into(),
                pid: 3,
                restarts: 0,
            },
            Service {
                name: "monitor".into(),
                description: "Shows it all.".into(),
                state: "Waiting".into(),
                pid: 0,
                restarts: 1,
            },
        ])
    }
    fn boot(&mut self) -> Result<Vec<(String, f64)>, String> {
        Ok(vec![("F0".into(), 900.0), ("F1".into(), 1200.0), ("B0".into(), 0.0), ("S3".into(), 120.5)])
    }
    fn log(&mut self, service: Option<&str>, cursor: u64, _limit: u16) -> Result<(Vec<Record>, u64), String> {
        let all = vec![
            Record { ms: 1500.0, service: "stage".into(), level: 2, msg: "first frame".into() },
            Record { ms: 1600.0, service: "desktop".into(), level: 3, msg: "hm".into() },
        ];
        let keep: Vec<Record> = all.into_iter().filter(|r| service.is_none_or(|s| r.service == s)).collect();
        Ok((if cursor == 0 { keep } else { vec![] }, 7))
    }
    fn mem(&mut self) -> Result<(u64, u64), String> {
        Ok((512 << 20, 128 << 20))
    }
    fn uptime_ns(&mut self) -> u64 {
        (3 * 60 + 12) * 1_000_000_000
    }
    fn theme(&mut self, dark: bool) -> Result<(), String> {
        self.theme = Some(dark);
        Ok(())
    }
    fn scale(&mut self, pct: u16) -> Result<(), String> {
        self.scale = Some(pct);
        Ok(())
    }
    fn reboot(&mut self, _other: bool) -> Result<(), String> {
        Err("reboot: not yet: Freally OS can't restart the computer from inside yet.".into())
    }
    fn faults(&mut self) -> Result<Vec<Fault>, String> {
        Ok(vec![])
    }
    fn version(&self) -> String {
        "Freally OS 0.1.0 (e2e)".into()
    }
    fn restart(&mut self, service: &str) -> Result<(), String> {
        self.restarted.push(service.into());
        Ok(())
    }
    fn dev(&self) -> bool {
        self.dev
    }
    fn list(&mut self, path: &str) -> Result<Vec<(String, bool)>, String> {
        let prefix = if path.ends_with('/') { path.to_string() } else { format!("{path}/") };
        Ok(self
            .files
            .iter()
            .filter_map(|(p, v)| {
                p.strip_prefix(&prefix).filter(|r| !r.contains('/')).map(|r| (r.to_string(), v.is_none()))
            })
            .collect())
    }
    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, String> {
        match self.files.get(path) {
            Some(Some(b)) => Ok(b.clone()),
            Some(None) => Err("a directory isn't a file (WrongType)".into()),
            None if path.starts_with("/home/sam") => Err("not yours to reach (AccessDenied)".into()),
            None => Err("no such name (NotFound)".into()),
        }
    }
    fn write_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.files.insert(path.into(), Some(bytes.to_vec()));
        Ok(())
    }
    fn make_dir(&mut self, path: &str) -> Result<(), String> {
        self.files.insert(path.into(), None);
        Ok(())
    }
    fn remove(&mut self, path: &str) -> Result<(), String> {
        self.files.remove(path).map(|_| ()).ok_or_else(|| "no such name (NotFound)".into())
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let v = self.files.remove(from).ok_or("no such name (NotFound)")?;
        self.files.insert(to.into(), v);
        Ok(())
    }
    fn whoami(&mut self) -> Result<String, String> {
        Ok("tester".into())
    }
}

#[test]
fn the_file_commands_parse_and_say_what_happened() {
    assert_eq!(ok("ls"), Command::Ls { path: "/".into() });
    assert_eq!(ok("cat /home/tester/a.txt"), Command::Cat { path: "/home/tester/a.txt".into() });
    assert_eq!(
        ok("write /home/tester/a.txt hello there"),
        Command::Write { path: "/home/tester/a.txt".into(), text: "hello there".into() }
    );
    assert_eq!(ok("mv /a /b"), Command::Mv { from: "/a".into(), to: "/b".into() });
    assert_eq!(ok("whoami"), Command::Whoami);
    assert!(parse("cat notes.txt", true).unwrap_err().contains("paths start with /"));
    assert!(parse("write /x", true).unwrap_err().contains("usage: write <path> <text>"));
    let mut p = Pretend::default();
    let mut say = |line: &str| run(&ok(line), &mut p).text;
    assert_eq!(say("mkdir /home/tester/Docs"), "");
    assert_eq!(say("write /home/tester/Docs/a.txt hello there"), "");
    assert_eq!(say("cat /home/tester/Docs/a.txt"), "hello there\n");
    assert_eq!(say("ls /home/tester"), "Docs/\n");
    assert_eq!(say("ls /home/tester/Docs"), "a.txt\n");
    assert_eq!(say("mv /home/tester/Docs/a.txt /home/tester/Docs/b.txt"), "");
    assert_eq!(say("cat /home/sam/secret"), "cat: /home/sam/secret: not yours to reach (AccessDenied)\n");
    assert_eq!(say("rm /home/tester/Docs/b.txt"), "");
    assert_eq!(say("ls /home/tester/Docs"), "(empty)\n");
    assert_eq!(say("whoami"), "tester\n");
}

fn ok(line: &str) -> Command {
    parse(line, true).unwrap().unwrap()
}

#[test]
fn each_parser() {
    assert_eq!(parse("   ", false), Ok(None));
    assert_eq!(ok("help"), Command::Help(None));
    assert_eq!(ok("help ps"), Command::Help(Some("ps".into())));
    assert_eq!(ok("ps"), Command::Ps);
    assert_eq!(ok("boot"), Command::Boot);
    assert_eq!(ok("log"), Command::Log { service: None, follow: false });
    assert_eq!(ok("log stage -f"), Command::Log { service: Some("stage".into()), follow: true });
    assert_eq!(ok("log -f"), Command::Log { service: None, follow: true });
    assert_eq!(ok("mem"), Command::Mem);
    assert_eq!(ok("uptime"), Command::Uptime);
    assert_eq!(ok("clear"), Command::Clear);
    assert_eq!(ok("theme light"), Command::Theme { dark: false });
    assert_eq!(ok("scale 150"), Command::Scale { pct: 150 });
    assert_eq!(ok("scale 125%"), Command::Scale { pct: 125 });
    assert_eq!(ok("reboot"), Command::Reboot { other: false });
    assert_eq!(ok("reboot other"), Command::Reboot { other: true });
    assert_eq!(ok("faults"), Command::Faults);
    assert_eq!(ok("version"), Command::Version);
    assert_eq!(ok("restart logger"), Command::Restart { service: "logger".into() });
}

#[test]
fn every_one_answers_help() {
    for (name, ..) in BUILTINS {
        assert_eq!(ok(&format!("{name} --help")), Command::Help(Some((*name).into())), "{name}");
        assert!(help(Some(name), true).contains(name), "{name}");
    }
    assert_eq!(ok("scale -h"), Command::Help(Some("scale".into())));
}

#[test]
fn mistakes_say_how() {
    assert!(parse("ps all", false).unwrap_err().contains("usage: ps"));
    assert!(parse("theme blue", false).unwrap_err().contains("usage: theme dark|light"));
    assert!(parse("scale 110", false).unwrap_err().contains("steps of 25"));
    assert!(parse("scale 400", false).unwrap_err().contains("steps of 25"));
    assert!(parse("log a b", false).unwrap_err().contains("usage: log"));
    assert!(parse("reboot now", false).unwrap_err().contains("usage: reboot"));
    assert!(parse("frob", false).unwrap_err().contains("no such command"));
    assert!(parse("restart logger", false).unwrap_err().contains("no such command"), "restart is dev only");
    assert!(!help(None, false).contains("restart <service>"), "not listed outside dev images");
    assert!(help(None, true).contains("restart <service>"));
}

#[test]
fn what_they_print() {
    let mut s = Pretend { dev: true, ..Pretend::default() };
    let ps = run(&Command::Ps, &mut s).text;
    assert!(ps.starts_with("SERVICE") && ps.contains("logger") && ps.contains("Keeps the log."));
    assert!(ps.lines().nth(2).unwrap().contains("    - "), "no process: a dash");
    let boot = run(&Command::Boot, &mut s).text;
    assert!(boot.contains("S3      120.5 ms") && boot.contains("B0 to S3: 120.5 ms") && boot.contains("F0 900.0 ms"));
    assert_eq!(run(&Command::Mem, &mut s).text, "128.0 MiB used of 512.0 MiB (25%)\n");
    assert_eq!(run(&Command::Uptime, &mut s).text, "up 3 min 12 s\n");
    assert_eq!(run(&Command::Faults, &mut s).text, "Nothing has crashed since Freally OS started.\n");
    assert_eq!(run(&Command::Version, &mut s).text, "Freally OS 0.1.0 (e2e)\n");
    assert!(run(&Command::Clear, &mut s).clear);
    let log = run(&Command::Log { service: Some("stage".into()), follow: true }, &mut s);
    assert!(log.text.contains("first frame") && !log.text.contains("hm"));
    assert_eq!(log.follow, Some(Follow { service: Some("stage".into()), cursor: 7 }));
    run(&Command::Theme { dark: false }, &mut s);
    run(&Command::Scale { pct: 200 }, &mut s);
    run(&Command::Restart { service: "logger".into() }, &mut s);
    assert_eq!((s.theme, s.scale, s.restarted.as_slice()), (Some(false), Some(200), &["logger".to_string()][..]));
    assert!(run(&Command::Reboot { other: false }, &mut s).text.starts_with("reboot: not yet"));
}

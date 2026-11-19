// SPDX-License-Identifier: Apache-2.0
//! Monitor's data, worked out by hand.

use super::*;
use alloc::string::ToString;
use alloc::vec;

fn p(name: &str, cpu_ns: u64, mem: u64, restarts: u32) -> Proc {
    Proc {
        name: name.into(),
        description: "x".into(),
        state: "Running".into(),
        pid: 1,
        restarts,
        mem_bytes: mem,
        cpu_ns,
    }
}

#[test]
fn cpu_comes_from_two_samples() {
    let mut ps = Processes::default();
    ps.sample(vec![p("stage", 1_000_000, 0, 0), p("logger", 0, 0, 0)], 1_000_000_000);
    assert!(ps.rows.iter().all(|r| r.cpu_pct.is_none()), "one sample: no CPU % yet");
    assert_eq!(cpu(ps.rows[0].cpu_pct), "—");
    // A second later: stage ran 250 ms of it.
    ps.sample(vec![p("stage", 251_000_000, 0, 0), p("logger", 10_000_000, 0, 0)], 2_000_000_000);
    let stage = ps.rows.iter().find(|r| r.proc.name == "stage").unwrap();
    assert_eq!(stage.cpu_pct, Some(25.0));
    assert_eq!(cpu(stage.cpu_pct), "25.0 %");
    assert_eq!(ps.rows[0].proc.name, "stage", "sorted by CPU, most first");
}

#[test]
fn sorting_by_each_column_either_way() {
    let mut ps = Processes::default();
    ps.sample(vec![p("b", 0, 300, 2), p("a", 0, 100, 0), p("c", 0, 200, 5)], 1);
    ps.sort_by(Column::Name);
    let names = |ps: &Processes| ps.rows.iter().map(|r| r.proc.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&ps), ["a", "b", "c"]);
    ps.sort_by(Column::Name);
    assert_eq!(names(&ps), ["c", "b", "a"], "again: the other way");
    ps.sort_by(Column::Memory);
    assert_eq!(names(&ps), ["b", "c", "a"], "memory: most first");
    ps.sort_by(Column::Restarts);
    assert_eq!(names(&ps), ["c", "b", "a"]);
}

#[test]
fn bytes_read_naturally() {
    assert_eq!(bytes(512), "1 KiB");
    assert_eq!(bytes(640 * 1024), "640 KiB");
    assert_eq!(bytes(12 * 1024 * 1024 + 300 * 1024), "12.3 MiB");
}

#[test]
fn the_boot_stage_by_stage_with_the_firmware_apart() {
    let stages: Vec<(String, f64)> =
        [("F0", 3000.0), ("F1", 4100.0), ("B0", 0.0), ("K0", 60.0), ("S3", 120.0), ("S4", 200.0)]
            .iter()
            .map(|(n, ms)| (n.to_string(), *ms))
            .collect();
    let b = Boot::of(&stages);
    assert_eq!(b.stages.len(), 4);
    assert_eq!(b.stages[1], ("K0".to_string(), 60.0, 60.0));
    assert_eq!(b.stages[3], ("S4".to_string(), 200.0, 80.0));
    assert_eq!(b.total_ms, 200.0);
    assert_eq!(b.share(3), 0.4);
    assert_eq!(b.summary(), "Firmware 4.1 s · Freally OS 0.2 s");
    // With the firmware's vendor (BootInfo v3, TASK-073).
    let hp = b.clone().with_vendor("HP");
    assert_eq!(hp.summary(), "Firmware (HP) 4.1 s · Freally OS 0.2 s");
    let none = Boot::of(&stages[2..]);
    assert!(none.summary().contains("doesn't report"));
    assert_eq!(
        none.with_vendor("EDK II").summary(),
        "Freally OS 0.2 s (the EDK II firmware doesn't report its own time)"
    );
}

#[test]
fn devices_list_what_each_driver_took() {
    let s = |a: &str, b: &str| (a.to_string(), b.to_string());
    let pci = [s("00:00.0", "Host bridge"), s("00:01.0", "Display (VGA)")];
    let bound = [
        s("framebuffer", "drv-gop-fb"),
        s("00:01.0", "drv-gop-fb"),
        s("PS/2 controller", "drv-i8042"),
        s("COM2", "drv-uart16550"),
    ];
    let rows = device_rows(&pci, &bound);
    let at: Vec<&str> = rows.iter().map(|r| r.at.as_str()).collect();
    // The devices off PCI first, then PCI by address.
    assert_eq!(at, ["framebuffer", "PS/2 controller", "COM2", "00:00.0", "00:01.0"]);
    assert_eq!(rows[1].what, "Keyboard and mouse controller (8042)");
    assert_eq!(rows[2].what, "Serial port");
    assert_eq!(rows[3].driver, "—");
    assert_eq!(rows[4].driver, "drv-gop-fb");
}

#[test]
fn the_log_filters_and_keeps_the_last_2000() {
    let mut log = Log::default();
    for i in 0..LOG_KEEP + 5 {
        log.push(Line {
            ms: i as f64,
            service: if i % 2 == 0 { "stage".into() } else { "desktop".into() },
            level: (i % 5) as u8,
            msg: "m".into(),
        });
    }
    assert_eq!(log.lines.len(), LOG_KEEP);
    assert_eq!(log.lines[0].ms, 5.0, "the oldest went first");
    log.service = Some("stage".into());
    assert!(log.shown().iter().all(|l| l.service == "stage"));
    log.min_level = 3;
    assert!(log.shown().iter().all(|l| l.service == "stage" && l.level >= 3));
    assert_eq!(log.services(), ["desktop", "stage"]);
}

#[test]
fn a_fault_expands() {
    let f = FaultRow {
        seq: 3,
        service: "drv-x".into(),
        kind: "PageFault".into(),
        message: "boom".into(),
        rip: 0x1000,
        addr: 0,
        frames: vec![0x2000],
        last_boot: false,
    };
    let d = fault_details(&f);
    assert_eq!(d[0], "PageFault in drv-x (#3)");
    assert_eq!(d[2], "at 0x1000, touching 0x0");
    assert!(d[3].contains("0x0000000000002000"));
    // The last boot's panic, filed at this boot from the last-panic page (TASK-074).
    let last = FaultRow { service: "freally-core".into(), kind: "Panic".into(), last_boot: true, ..f };
    assert_eq!(fault_details(&last)[0], "Panic in freally-core (#3): the last boot stopped here");
}

#[test]
fn the_state_keeps_the_newest_lines_that_fit_a_message() {
    let lines: Vec<String> = (0..5000).map(|i| alloc::format!("logger line {i} with some words in it")).collect();
    let kept = newest_that_fit(&lines, STATE_LINES_BYTES);
    let size: usize = kept.iter().map(|l| l.len() + 4).sum();
    assert!(size <= STATE_LINES_BYTES && kept.len() > 100, "{} lines, {size} bytes", kept.len());
    assert_eq!(kept.last(), lines.last(), "the newest kept");
    assert_eq!(newest_that_fit(&lines[..3], STATE_LINES_BYTES), &lines[..3], "a short log, whole");
}

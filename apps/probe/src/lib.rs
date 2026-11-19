// SPDX-License-Identifier: Apache-2.0
//! The probe (TASK-069, dev only): the first image to boot on a new machine.
//! It has no desktop and no disk driver; it draws straight to the
//! framebuffer everything Freally OS found (the CPU and its core types, the
//! APIC mode, the memory, the display's modes and EDID, the firmware's own
//! times, the ACPI tables, the PS/2 controller, every PCI function and the
//! driver that took it, every service, and every problem logged), page by
//! page, so a phone photograph is the record. No keyboard is needed: the
//! pages turn by themselves.
//!
//! This library is the layout, host-tested; `main.rs` gathers and draws.
#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// One row of a column: a section's heading, or one of its lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Heading(String),
    Line(String),
}

/// A titled list of lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub lines: Vec<String>,
}

impl Section {
    pub fn new(title: &str) -> Section {
        Section { title: String::from(title), lines: Vec::new() }
    }
    pub fn line(&mut self, s: impl Into<String>) {
        self.lines.push(s.into());
    }
}

/// The sections laid out in columns of `rows` rows, `cols` columns to a
/// page. A heading never ends a column (it moves on with its first line); a
/// section that runs on carries its heading into the next column, marked
/// "(continued)".
pub fn pages(sections: &[Section], rows: usize, cols: usize) -> Vec<Vec<Vec<Row>>> {
    let (rows, cols) = (rows.max(2), cols.max(1));
    let mut columns: Vec<Vec<Row>> = Vec::new();
    let mut col: Vec<Row> = Vec::new();
    for s in sections {
        if !col.is_empty() && rows - col.len() < 2 {
            columns.push(core::mem::take(&mut col));
        }
        col.push(Row::Heading(s.title.clone()));
        for l in &s.lines {
            if col.len() == rows {
                columns.push(core::mem::take(&mut col));
                col.push(Row::Heading(alloc::format!("{} (continued)", s.title)));
            }
            col.push(Row::Line(l.clone()));
        }
    }
    if !col.is_empty() {
        columns.push(col);
    }
    let mut out = Vec::new();
    let mut it = columns.into_iter().peekable();
    while it.peek().is_some() {
        out.push(it.by_ref().take(cols).collect());
    }
    out
}

/// Long lists (APIC IDs, GOP modes, table signatures) wrapped `per` to a line.
pub fn wrap(items: &[String], per: usize) -> Vec<String> {
    items.chunks(per.max(1)).map(|c| c.join("  ")).collect()
}

/// What the probe says about the keyboard: the decision TASK-069 is for
/// (the PS/2 controller and its driver, or USB first: TASK-072A–E).
pub fn keyboard_verdict(has_8042: bool, driver_state: Option<&str>) -> String {
    match (has_8042, driver_state) {
        (false, _) => String::from(
            "The firmware says there's no PS/2 controller: the keyboard needs the USB driver first (TASK-072A–E).",
        ),
        (true, Some("Running")) => String::from("PS/2 controller present and drv-i8042 running: the keyboard is PS/2."),
        (true, Some("Starting" | "Waiting")) => String::from(
            "The firmware says there's a PS/2 controller; drv-i8042 is still starting (the next round says).",
        ),
        (true, Some(s)) => {
            alloc::format!("The firmware says there's a PS/2 controller, but drv-i8042 is {s}: see the problems below.")
        }
        (true, None) => {
            String::from("The firmware says there's a PS/2 controller, but drv-i8042 hasn't started: see the problems.")
        }
    }
}

/// "12.3 ms" from nanoseconds.
pub fn ms(ns: u64) -> String {
    alloc::format!("{}.{} ms", ns / 1_000_000, (ns / 100_000) % 10)
}

#[cfg(test)]
mod tests;

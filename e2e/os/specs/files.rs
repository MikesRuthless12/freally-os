// SPDX-License-Identifier: Apache-2.0
//! Files, v0 (TASK-105, FR-049), on a GPT disk with the Owner logged in,
//! its files made through Terminal: the list (folders first), a folder
//! opened by double-click and by Enter, Backspace and Up back; the grid;
//! a new folder named as it's made; rename (with its refusals); delete to
//! the bin, restore, empty; Ctrl+N's second window; a row dragged onto a
//! folder in its own window, and onto another window.

use crate::Ctx;
use crate::session::{press, sel, typed};
use crate::settings::signed_in;
use crate::stage::{button, key, list, move_to, win};
use crate::sweep::{attr, box_of, click, q, q_all, until_attr, until_gone, until_q};
use crate::terminal::{expect, once};
use std::time::{Duration, Instant};

const FILES: &str = "WINDOW[data-owner=\"files\"]";

fn row(name: &str) -> String {
    sel(&format!("files-row-{name}"))
}

/// `testid` in window `id`.
fn in_win(id: u32, testid: &str) -> String {
    format!("WINDOW[data-window-id=\"{id}\"] [data-testid=\"{testid}\"]")
}

fn with(ctx: &mut Ctx, held: &str, code: &str) -> Result<(), String> {
    key(ctx, held, true)?;
    press(ctx, code)?;
    key(ctx, held, false)
}

fn press_id(ctx: &mut Ctx, selector: &str) -> Result<(), String> {
    let n = until_q(ctx, selector)?;
    click(ctx, n)
}

fn backspace(ctx: &mut Ctx, n: usize) -> Result<(), String> {
    (0..n).try_for_each(|_| press(ctx, "Backspace"))
}

/// Files' windows' ids, oldest first.
fn windows(ctx: &mut Ctx) -> Result<Vec<u32>, String> {
    let mut ids = Vec::new();
    for n in q_all(ctx, FILES)? {
        ids.push(attr(ctx, n, "data-window-id")?.and_then(|v| v.parse().ok()).ok_or("a window with no id")?);
    }
    ids.sort_unstable();
    Ok(ids)
}

/// Files open (Super+E, as Windows has it) in the home.
fn open_files(ctx: &mut Ctx) -> Result<(), String> {
    if q(ctx, &sel("files-list"))? == 0 {
        with(ctx, "MetaLeft", "KeyE")?;
        until_q(ctx, &sel("files-list"))?;
    }
    // The Home place: the home, from a folder or the bin.
    press_id(ctx, &sel("files-place-home"))?;
    until_attr(ctx, &sel("files-path"), "aria-valuenow", "/home/mike")
}

/// The shell makes what the specs work on (each only once). A big
/// `mkfile` keeps the Terminal busy past the door's answer: it's waited out.
fn made(ctx: &mut Ctx, lines: &[&str]) -> Result<(), String> {
    crate::terminal::open(ctx)?;
    for l in lines {
        if l.starts_with("mkfile") {
            slowly(ctx, l, 240)?;
        } else {
            once(ctx, l, &[])?;
        }
    }
    Ok(())
}

/// `line` run in the Terminal, waited for (`secs`) until its prompt is back:
/// the screen's words after it.
fn slowly(ctx: &Ctx, line: &str, secs: u64) -> Result<String, String> {
    crate::terminal::feed(ctx, "type", b"clear\r")?;
    std::thread::sleep(Duration::from_millis(200));
    crate::terminal::feed(ctx, "type", format!("{line}\r").as_bytes())?;
    let start = Instant::now();
    let mut seen: String;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        // Busy, the Terminal doesn't answer the door: asked again.
        match crate::terminal::screen_text(ctx) {
            Ok(s) => {
                let lines: Vec<&str> = s.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
                if lines.len() >= 2 && lines.last() == Some(&"freally:~$") && s.contains(line) {
                    return Ok(s);
                }
                seen = s;
            }
            Err(e) => seen = format!("(the screen: {e})"),
        }
        if start.elapsed() > Duration::from_secs(secs) {
            return Err(format!("`{line}` didn't finish in {secs} s; the screen last:\n{seen}"));
        }
    }
}

/// A double-click where the pointer is.
fn double(ctx: &mut Ctx, selector: &str) -> Result<(), String> {
    let n = until_q(ctx, selector)?;
    let (x0, y0, x1, y1) = box_of(ctx, n)?;
    move_to(ctx, ((x0 + x1) / 2) as i32, ((y0 + y1) / 2) as i32)?;
    for _ in 0..2 {
        button(ctx, true)?;
        button(ctx, false)?;
    }
    Ok(())
}

/// `@files.browse`: the home's contents, folders first; a folder opened by
/// double-click and by Enter, Backspace and Up back to the home (the
/// folder it left chosen); the grid and back.
pub fn browse(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    made(
        ctx,
        &[
            "mkdir /home/mike/Projects",
            "write /home/mike/Projects/plan.txt the plan",
            "write /home/mike/notes.txt hello",
        ],
    )?;
    open_files(ctx)?;
    until_q(ctx, &row("notes.txt"))?;
    let mut order = Vec::new();
    for n in q_all(ctx, &format!("{FILES} OPTION"))? {
        if let Some(id) = attr(ctx, n, "data-testid")?.filter(|t| t.starts_with("files-row-")) {
            order.push(id);
        }
    }
    let at = |name: &str| order.iter().position(|o| o == &format!("files-row-{name}"));
    if !(at("Projects") < at("notes.txt") && at("Projects").is_some()) {
        return Err(format!("not folders first: {order:?}"));
    }
    until_attr(ctx, &row("Projects"), "aria-valuenow", "folder")?;
    until_attr(ctx, &row("notes.txt"), "aria-valuenow", "6 bytes")?;
    double(ctx, &row("Projects"))?;
    until_attr(ctx, &sel("files-path"), "aria-valuenow", "/home/mike/Projects")?;
    until_q(ctx, &row("plan.txt"))?;
    press(ctx, "Backspace")?;
    until_attr(ctx, &sel("files-path"), "aria-valuenow", "/home/mike")?;
    until_attr(ctx, &row("Projects"), "aria-selected", "true")?;
    press(ctx, "Enter")?;
    until_attr(ctx, &sel("files-path"), "aria-valuenow", "/home/mike/Projects")?;
    press_id(ctx, &sel("files-up"))?;
    until_attr(ctx, &sel("files-path"), "aria-valuenow", "/home/mike")?;
    until_attr(ctx, &sel("files-up"), "aria-disabled", "true")?;
    press_id(ctx, &sel("files-view"))?;
    until_attr(ctx, &sel("files-view"), "aria-label", "List")?;
    until_attr(ctx, &row("Projects"), "aria-valuenow", "folder")?;
    ctx.keep_shot("files-grid")?;
    press_id(ctx, &sel("files-view"))?;
    until_attr(ctx, &sel("files-view"), "aria-label", "Grid")?;
    ctx.keep_shot("files-list")
}

/// `@files.edit`: a new folder named as it's made; rename with F2 (an
/// empty name and a taken one refused) and with the Rename key (Escape
/// leaves it); Delete to the bin, Restore, and Empty the bin for good.
pub fn edit(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    made(ctx, &["write /home/mike/draft.txt words"])?;
    open_files(ctx)?;
    // A new folder.
    press_id(ctx, &sel("files-new-folder"))?;
    until_attr(ctx, &sel("files-name"), "aria-valuenow", "New folder")?;
    backspace(ctx, 10)?;
    typed(ctx, "Drafts")?;
    press(ctx, "Enter")?;
    until_q(ctx, &row("Drafts"))?;
    until_gone(ctx, &sel("files-name"))?;
    // Rename with F2.
    press_id(ctx, &row("draft.txt"))?;
    press(ctx, "F2")?;
    until_attr(ctx, &sel("files-name"), "aria-valuenow", "draft.txt")?;
    backspace(ctx, 9)?;
    press(ctx, "Enter")?;
    until_attr(ctx, &sel("files-error"), "aria-label", "A name can't be empty.")?;
    typed(ctx, "Drafts")?;
    press(ctx, "Enter")?;
    until_attr(ctx, &sel("files-error"), "aria-label", "There's already something called Drafts here.")?;
    backspace(ctx, 6)?;
    typed(ctx, "letter")?;
    press(ctx, "Enter")?;
    until_q(ctx, &row("letter"))?;
    until_gone(ctx, &row("draft.txt"))?;
    press_id(ctx, &sel("files-rename"))?;
    until_attr(ctx, &sel("files-name"), "aria-valuenow", "letter")?;
    typed(ctx, "x")?;
    press(ctx, "Escape")?;
    until_gone(ctx, &sel("files-name"))?;
    until_q(ctx, &row("letter"))?;
    // Delete to the bin, and back.
    press(ctx, "Delete")?;
    until_gone(ctx, &row("letter"))?;
    press_id(ctx, &sel("files-place-bin"))?;
    until_attr(ctx, &row("letter"), "aria-valuenow", "from /home/mike")?;
    press_id(ctx, &sel("files-restore"))?;
    until_gone(ctx, &row("letter"))?;
    press_id(ctx, &sel("files-place-home"))?;
    until_q(ctx, &row("letter"))?;
    // Delete, and empty the bin for good.
    press_id(ctx, &row("letter"))?;
    press_id(ctx, &sel("files-delete"))?;
    until_gone(ctx, &row("letter"))?;
    press_id(ctx, &sel("files-place-bin"))?;
    until_q(ctx, &row("letter"))?;
    press_id(ctx, &sel("files-empty"))?;
    until_gone(ctx, &row("letter"))?;
    until_attr(ctx, &sel("files-empty"), "aria-disabled", "true")?;
    expect(ctx, "ls /home/mike/.bin", &["(empty)"])?;
    press_id(ctx, &sel("files-place-home"))?;
    until_q(ctx, &row("Drafts"))?;
    Ok(())
}

/// Drags with the real mouse from (x, y) to (tx, ty).
fn drag(ctx: &mut Ctx, (x, y): (i32, i32), (tx, ty): (i32, i32)) -> Result<(), String> {
    move_to(ctx, x, y)?;
    button(ctx, true)?;
    move_to(ctx, x + 12, y)?;
    move_to(ctx, tx, ty)?;
    button(ctx, false)
}

fn centre(ctx: &mut Ctx, selector: &str) -> Result<(i32, i32), String> {
    let n = until_q(ctx, selector)?;
    let (x0, y0, x1, y1) = box_of(ctx, n)?;
    Ok((((x0 + x1) / 2) as i32, ((y0 + y1) / 2) as i32))
}

/// `@files.drag`: a row dragged onto a folder in its own window moves into
/// it; Ctrl+N opens a second window; a row dragged from one window onto
/// the other (stage carries it) moves into the folder that one shows;
/// Alt+F4 closes them one at a time.
pub fn drag_spec(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    made(ctx, &["mkdir /home/mike/Box", "mkdir /home/mike/Shelf", "write /home/mike/loose.txt here"])?;
    open_files(ctx)?;
    // Within a window: onto a folder's row.
    until_q(ctx, &row("loose.txt"))?;
    let (lx, ly) = centre(ctx, &row("loose.txt"))?;
    let to = centre(ctx, &row("Box"))?;
    drag(ctx, (lx - 60, ly), to)?;
    until_gone(ctx, &row("loose.txt"))?;
    expect(ctx, "ls /home/mike/Box", &["loose.txt"])?;
    // A second window, moved aside by its strip.
    with(ctx, "ControlLeft", "KeyN")?;
    let start = Instant::now();
    while windows(ctx)?.len() < 2 {
        if start.elapsed() > Duration::from_secs(10) {
            return Err("Ctrl+N opened no second window".into());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    let ids = windows(ctx)?;
    let (a, b) = (ids[0], ids[1]);
    let (ws, _) = list(ctx)?;
    let wb = win(&ws, b)?;
    drag(ctx, (wb.x + 60, wb.y + 12), (wb.x + 60 + 340, wb.y + 12))?;
    // b shows Shelf.
    double(ctx, &in_win(b, "files-row-Shelf"))?;
    until_attr(ctx, &in_win(b, "files-path"), "aria-valuenow", "/home/mike/Shelf")?;
    // a's Box row onto b's list: into Shelf.
    // Pressed near the row's left end: the second window covers its right.
    let n = until_q(ctx, &in_win(a, "files-row-Box"))?;
    let (rx, ry0, _, ry1) = box_of(ctx, n)?;
    let (bx, by) = (rx as i32 + 160, ((ry0 + ry1) / 2) as i32);
    let n = until_q(ctx, &in_win(b, "files-list"))?;
    let (x0, _, _, y1) = box_of(ctx, n)?;
    // The press raises the first window over the second: let go where the
    // second still shows, right of the first one's edge.
    let wa = win(&list(ctx)?.0, a)?;
    let drop_x = (wa.x + wa.w + 60).max(x0 as i32 + 120);
    drag(ctx, (bx, by), (drop_x, y1 as i32 - 40))?;
    until_q(ctx, &in_win(b, "files-row-Box"))?;
    expect(ctx, "ls /home/mike/Shelf", &["Box/"])?;
    expect(ctx, "ls /home/mike/Shelf/Box", &["loose.txt"])?;
    ctx.keep_shot("files-two-windows")?;
    // Alt+F4 closes the focused one; the other stays, then goes too.
    with(ctx, "AltLeft", "F4")?;
    let start = Instant::now();
    while windows(ctx)?.len() != 1 {
        if start.elapsed() > Duration::from_secs(10) {
            return Err("Alt+F4 didn't close a Files window".into());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    let closed = ctx.log_count("files: closed");
    with(ctx, "AltLeft", "F4")?;
    ctx.expect_log_count("files: closed", closed + 1)
}

// ---- The copy engine and its window (TASK-105A) ---------------------------

const COPY_RUN: &str = "WINDOW[data-owner=\"files\"] GROUP";

/// The copy window's running jobs' numbers (from their `copy-run-<job>` ids).
fn jobs(ctx: &mut Ctx) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for n in q_all(ctx, COPY_RUN)? {
        if let Some(j) =
            attr(ctx, n, "data-testid")?.and_then(|t| t.strip_prefix("copy-run-").and_then(|j| j.parse().ok()))
        {
            out.push(j);
        }
    }
    Ok(out)
}

/// Waits (`secs`) for a job in the copy window; its number.
fn a_job(ctx: &mut Ctx, secs: u64) -> Result<u32, String> {
    let start = Instant::now();
    loop {
        if let Some(&j) = jobs(ctx)?.first() {
            return Ok(j);
        }
        if start.elapsed() > Duration::from_secs(secs) {
            return Err("no copy showed in the copy window".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The copy jobs vfs has said finished (done or cancelled).
fn finished(ctx: &Ctx) -> Vec<String> {
    ctx.log_lines_with("vfs: copy ")
        .into_iter()
        .filter(|l| l.contains(" done: ") || l.contains(" cancelled: "))
        .collect()
}

/// Waits (60 s) for a copy job to finish after the `before` already said; what vfs said.
fn copies_done(ctx: &Ctx, before: usize) -> Result<String, String> {
    let start = Instant::now();
    loop {
        let f = finished(ctx);
        if f.len() > before {
            return Ok(f.last().cloned().unwrap_or_default());
        }
        if start.elapsed() > Duration::from_secs(60) {
            return Err("vfs never said the copy finished".into());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// `@files.copy.paste`: Ctrl+C a folder, Ctrl+V into another: copied whole,
/// by reflink (nothing written but the new files' records); Ctrl+X and
/// Ctrl+V move it, and Ctrl+Z puts it back.
pub fn copy_paste(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    made(
        ctx,
        &[
            "mkdir /home/mike/Src",
            "write /home/mike/Src/a.txt alpha",
            "write /home/mike/Src/b.txt beta",
            "mkdir /home/mike/Dst",
        ],
    )?;
    open_files(ctx)?;
    press_id(ctx, &row("Src"))?;
    with(ctx, "ControlLeft", "KeyC")?;
    ctx.expect_log("files: copied /home/mike/Src")?;
    double(ctx, &row("Dst"))?;
    until_attr(ctx, &sel("files-path"), "aria-valuenow", "/home/mike/Dst")?;
    let before = finished(ctx).len();
    with(ctx, "ControlLeft", "KeyV")?;
    let said = copies_done(ctx, before)?;
    if !said.contains("done: 3 of 3 item(s)") || !said.contains("2 by reflink") {
        return Err(format!("vfs said {said:?}"));
    }
    until_q(ctx, &row("Src"))?;
    expect(ctx, "cat /home/mike/Dst/Src/b.txt", &["beta"])?;
    expect(ctx, "cat /home/mike/Src/b.txt", &["beta"])?;
    // Ctrl+X, Ctrl+V: a move; Ctrl+Z: back.
    press_id(ctx, &sel("files-place-home"))?;
    press_id(ctx, &row("Src"))?;
    with(ctx, "ControlLeft", "KeyX")?;
    ctx.expect_log("files: cut /home/mike/Src")?;
    made(ctx, &["mkdir /home/mike/Moved"])?;
    // Files reads the folder again (it didn't see the Terminal make one).
    press_id(ctx, &sel("files-place-home"))?;
    double(ctx, &row("Moved"))?;
    let before = finished(ctx).len();
    with(ctx, "ControlLeft", "KeyV")?;
    copies_done(ctx, before)?;
    expect(ctx, "ls /home/mike/Moved", &["Src/"])?;
    expect(ctx, "ls /home/mike", &["Moved/"])?;
    with(ctx, "ControlLeft", "KeyZ")?;
    ctx.expect_log("files: undid the move of Src")?;
    expect(ctx, "ls /home/mike/Moved", &["(empty)"])?;
    expect(ctx, "cat /home/mike/Src/a.txt", &["alpha"])?;
    Ok(())
}

/// `@files.copy.conflict`: the same folder pasted again: each taken name
/// asks; Skip one, *Let me decide for each file* (both kept: Keep both), and
/// another paste with *Do this for all conflicts* and Replace.
pub fn copy_conflict(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    made(
        ctx,
        &[
            "mkdir /home/mike/Clash",
            "write /home/mike/Clash/one.txt new one",
            "write /home/mike/Clash/two.txt new two",
            "write /home/mike/Clash/three.txt new three",
            "mkdir /home/mike/Shelf2",
            "mkdir /home/mike/Shelf2/Clash",
            "write /home/mike/Shelf2/Clash/one.txt old one",
            "write /home/mike/Shelf2/Clash/two.txt old two",
            "write /home/mike/Shelf2/Clash/three.txt old three",
        ],
    )?;
    open_files(ctx)?;
    press_id(ctx, &row("Clash"))?;
    with(ctx, "ControlLeft", "KeyC")?;
    double(ctx, &row("Shelf2"))?;
    with(ctx, "ControlLeft", "KeyV")?;
    let job = a_job(ctx, 10)?;
    until_attr(ctx, &sel(&format!("copy-run-{job}")), "aria-valuenow", "conflict")?;
    until_q(ctx, &sel("copy-conflict"))?;
    // The folders merge; its first file (one.txt) asks first: Skip.
    until_attr(
        ctx,
        &sel("copy-conflict"),
        "aria-label",
        "The destination already has a file named \u{201C}one.txt\u{201D}.",
    )?;
    press_id(ctx, &sel("copy-skip"))?;
    until_attr(
        ctx,
        &sel("copy-conflict"),
        "aria-label",
        "The destination already has a file named \u{201C}three.txt\u{201D}.",
    )?;
    // Let me decide: three.txt and two.txt, each with its two boxes.
    press_id(ctx, &sel("copy-decide"))?;
    until_attr(ctx, &sel(&format!("copy-run-{job}")), "aria-valuenow", "deciding")?;
    until_attr(ctx, &sel("copy-keep-new-two.txt"), "aria-checked", "true")?;
    until_attr(ctx, &sel("copy-keep-there-two.txt"), "aria-checked", "false")?;
    // two.txt: both kept (Keep both); three.txt: only the one there (Skip).
    press_id(ctx, &sel("copy-keep-there-two.txt"))?;
    until_attr(ctx, &sel("copy-keep-there-two.txt"), "aria-checked", "true")?;
    press_id(ctx, &sel("copy-keep-new-three.txt"))?;
    press_id(ctx, &sel("copy-keep-there-three.txt"))?;
    ctx.keep_shot("files-let-me-decide")?;
    let before = finished(ctx).len();
    press_id(ctx, &sel("copy-continue"))?;
    copies_done(ctx, before)?;
    expect(ctx, "cat /home/mike/Shelf2/Clash/one.txt", &["old one"])?;
    expect(ctx, "cat /home/mike/Shelf2/Clash/two.txt", &["old two"])?;
    // "two (2).txt": a name with a space, which the shell's cat can't take.
    expect(ctx, "ls /home/mike/Shelf2/Clash", &["two (2).txt"])?;
    expect(ctx, "cat /home/mike/Shelf2/Clash/three.txt", &["old three"])?;
    // Again, for all: Replace.
    with(ctx, "ControlLeft", "KeyV")?;
    let job = a_job(ctx, 10)?;
    until_attr(ctx, &sel(&format!("copy-run-{job}")), "aria-valuenow", "conflict")?;
    press_id(ctx, &sel("copy-all"))?;
    until_attr(ctx, &sel("copy-all"), "aria-checked", "true")?;
    let before = finished(ctx).len();
    press_id(ctx, &sel("copy-replace"))?;
    copies_done(ctx, before)?;
    for (f, want) in [("one.txt", "new one"), ("two.txt", "new two"), ("three.txt", "new three")] {
        expect(ctx, &format!("cat /home/mike/Shelf2/Clash/{f}"), &[want])?;
    }
    Ok(())
}

/// `@files.copy.pause`: a 48 MiB file pasted with verify (Ctrl+Shift+V: through
/// the buffer, read back): its window's progress, More details (the speed
/// graph), Pause (nothing moves) and Resume, done and the same size; then
/// another, cancelled part-way: no half file left.
pub fn copy_pause(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    made(ctx, &["mkdir /home/mike/Big", "mkfile /home/mike/Big/big.bin 48M", "mkdir /home/mike/Out"])?;
    open_files(ctx)?;
    double(ctx, &row("Big"))?;
    press_id(ctx, &row("big.bin"))?;
    until_attr(ctx, &row("big.bin"), "aria-valuenow", "50.3 MB")?;
    with(ctx, "ControlLeft", "KeyC")?;
    press_id(ctx, &sel("files-place-home"))?;
    double(ctx, &row("Out"))?;
    key(ctx, "ControlLeft", true)?;
    with(ctx, "ShiftLeft", "KeyV")?;
    key(ctx, "ControlLeft", false)?;
    let job = a_job(ctx, 10)?;
    press_id(ctx, &sel(&format!("copy-details-{job}")))?;
    until_q(ctx, &sel(&format!("copy-graph-{job}")))?;
    press_id(ctx, &sel(&format!("copy-pause-{job}")))?;
    until_attr(ctx, &sel(&format!("copy-run-{job}")), "aria-valuenow", "paused")?;
    let words = |ctx: &mut Ctx| -> Result<String, String> {
        let n = until_q(ctx, &sel(&format!("copy-words-{job}")))?;
        Ok(attr(ctx, n, "aria-valuenow")?.unwrap_or_default())
    };
    let a = words(ctx)?;
    std::thread::sleep(Duration::from_millis(1500));
    let b = words(ctx)?;
    if a != b || !b.ends_with("Paused") {
        return Err(format!("paused, it moved on: {a:?} then {b:?}"));
    }
    ctx.keep_shot("files-copy-paused")?;
    let before = finished(ctx).len();
    press_id(ctx, &sel(&format!("copy-pause-{job}")))?;
    let said = copies_done(ctx, before)?;
    if !said.contains("done: 1 of 1 item(s), 50331648 bytes, 0 by reflink") {
        return Err(format!("vfs said {said:?}"));
    }
    until_attr(ctx, &row("big.bin"), "aria-valuenow", "50.3 MB")?;
    // Another, cancelled part-way.
    press_id(ctx, &sel("files-place-home"))?;
    made(ctx, &["mkdir /home/mike/Out2"])?;
    press_id(ctx, &sel("files-place-home"))?;
    double(ctx, &row("Out2"))?;
    key(ctx, "ControlLeft", true)?;
    with(ctx, "ShiftLeft", "KeyV")?;
    key(ctx, "ControlLeft", false)?;
    let job = a_job(ctx, 10)?;
    let before = finished(ctx).len();
    press_id(ctx, &sel(&format!("copy-cancel-{job}")))?;
    let said = copies_done(ctx, before)?;
    if !said.contains("cancelled") {
        return Err(format!("vfs said {said:?}"));
    }
    expect(ctx, "ls /home/mike/Out2", &["(empty)"])?;
    Ok(())
}

/// `@files.copy.drag`: a row dragged onto a folder with Ctrl held is copied
/// (the source stays), through the engine.
pub fn copy_drag(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    // A folder of its own: by now the home is longer than the window, and a
    // row out of view has nothing on the screen to drag.
    made(ctx, &["mkdir /home/mike/Drag", "write /home/mike/Drag/keep.txt kept", "mkdir /home/mike/Drag/Copies"])?;
    open_files(ctx)?;
    double(ctx, &row("Drag"))?;
    until_attr(ctx, &sel("files-path"), "aria-valuenow", "/home/mike/Drag")?;
    let (kx, ky) = centre(ctx, &row("keep.txt"))?;
    let to = centre(ctx, &row("Copies"))?;
    let before = finished(ctx).len();
    key(ctx, "ControlLeft", true)?;
    drag(ctx, (kx - 60, ky), to)?;
    key(ctx, "ControlLeft", false)?;
    copies_done(ctx, before)?;
    expect(ctx, "cat /home/mike/Drag/Copies/keep.txt", &["kept"])?;
    until_q(ctx, &row("keep.txt"))?;
    Ok(())
}

/// A `cp` run in the Terminal: what it printed.
fn cp(ctx: &Ctx, args: &str) -> Result<String, String> {
    let s = slowly(ctx, &format!("cp {args}"), 300)?;
    s.lines().find(|l| l.starts_with("Copied ")).map(String::from).ok_or(format!("cp {args} said:\n{s}"))
}

/// `@bench.copy` (`cargo xtask bench copy`): MB/s for a big file (by reflink;
/// through the buffer and read back) and files/s for many small ones, in
/// QEMU, through `mkfile` and `cp`.
pub fn bench(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    made(
        ctx,
        &[
            "mkdir /home/mike/Bench",
            "mkdir /home/mike/Bench/small",
            "mkfile /home/mike/Bench/big.bin 64M",
            "mkdir /home/mike/Bench/o1",
            "mkdir /home/mike/Bench/o2",
            "mkdir /home/mike/Bench/o3",
            "mkdir /home/mike/Bench/o4",
        ],
    )?;
    for i in 0..200 {
        once(ctx, &format!("mkfile /home/mike/Bench/small/f{i:03} 4K"), &[])?;
    }
    for (what, args) in [
        ("a 64 MiB file by reflink", "/home/mike/Bench/big.bin /home/mike/Bench/o1"),
        ("a 64 MiB file through the buffer, read back", "--verify /home/mike/Bench/big.bin /home/mike/Bench/o2"),
        ("200 files of 4 KiB by reflink", "/home/mike/Bench/small /home/mike/Bench/o3"),
        ("200 files of 4 KiB through the buffer, read back", "--verify /home/mike/Bench/small /home/mike/Bench/o4"),
    ] {
        let said = cp(ctx, args)?;
        // "Copied N item(s), X in S s (R/s)…"
        let secs: f64 = said
            .split(" in ")
            .nth(1)
            .and_then(|r| r.split(" s ").next())
            .and_then(|s| s.parse().ok())
            .ok_or(format!("an unreadable `cp`: {said}"))?;
        let items: f64 = said.split_whitespace().nth(1).and_then(|n| n.parse().ok()).unwrap_or(0.0);
        let note = format!("bench copy: {what}: {said} [{:.0} files/s]", if secs > 0.0 { items / secs } else { 0.0 });
        ctx.note(&note);
    }
    Ok(())
}

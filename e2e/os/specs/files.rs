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

/// The shell makes what the specs work on (each only once).
fn made(ctx: &mut Ctx, lines: &[&str]) -> Result<(), String> {
    crate::terminal::open(ctx)?;
    for l in lines {
        once(ctx, l, &[])?;
    }
    Ok(())
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
/// empty name and a taken one refused, Escape leaves it); Delete to the
/// bin, Restore, and Empty the bin for good.
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
    press(ctx, "F2")?;
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

// SPDX-License-Identifier: Apache-2.0
//! The DOM's rules, each worked by hand: a Monitor window at (100, 50) with a
//! 32 px strip, so its surface starts at (100, 82); the Rack below it.

use super::*;
use alloc::vec;

fn win(id: u32, title: &str, owner: &str, z: u32, frame: (i32, i32, u32, u32), strip: u32) -> st::WindowInfo {
    let (x, y, w, h) = frame;
    st::WindowInfo {
        window_id: id,
        title: title.into(),
        kind: if strip > 0 { st::Kind::Normal } else { st::Kind::Panel },
        frame: st::Rect { x, y, w, h },
        focused: z == 1,
        z,
        owner: owner.into(),
        ax_bytes: 1,
        surface: st::Rect { x, y: y + strip as i32, w, h: h - strip },
    }
}

fn n(parent: u32, role: u8, name: &str, state: u32, testid: &str, at: (i32, i32, u32, u32)) -> ax::Node {
    ax::Node {
        parent,
        role,
        name: name.into(),
        state,
        value: String::new(),
        testid: testid.into(),
        x: at.0,
        y: at.1,
        w: at.2,
        h: at.3,
    }
}

fn desk() -> Doc {
    let windows =
        [win(7, "Monitor", "monitor", 1, (100, 50, 960, 672), 32), win(3, "Rack", "desktop", 0, (0, 744, 1280, 56), 0)];
    Doc::build(&windows, |id| match id {
        7 => Some(ax::Tree {
            nodes: vec![
                n(ax::ROOT, ax::TABLIST, "Tabs", 0, "", (16, 0, 560, 44)),
                n(0, ax::TAB, "Processes", ax::SELECTED, "tab-processes", (16, 0, 140, 44)),
                n(0, ax::TAB, "Boot", 0, "tab-boot", (156, 0, 140, 44)),
                n(ax::ROOT, ax::BUTTON, "Sort by Service", ax::PRESSED, "sort-name", (16, 56, 160, 24)),
            ],
        }),
        3 => Some(ax::Tree {
            nodes: vec![
                n(ax::ROOT, ax::BUTTON, "Launcher", 0, "rack-launcher", (8, 3, 40, 40)),
                n(ax::ROOT, ax::SWITCH, "Twelve-hour clock", ax::CHECKED, "", (1100, 8, 100, 40)),
                n(ax::ROOT, ax::TEXT, "6:29 AM", 0, "clock", (1110, 8, 90, 40)),
            ],
        }),
        _ => None,
    })
}

fn names(d: &Doc, ids: &[usize]) -> Vec<String> {
    ids.iter().map(|&i| d.nodes[i].attr("aria-label").unwrap_or(d.nodes[i].name).to_string()).collect()
}

#[test]
fn windows_stack_bottom_first_and_nodes_sit_on_the_screen() {
    let d = desk();
    let root = &d.nodes[0];
    assert_eq!(root.children.len(), 2);
    assert_eq!(d.nodes[root.children[0]].attr("data-owner"), Some("desktop"), "the Rack is lowest (z 0)");
    let tab = d.select(0, "[data-testid=\"tab-boot\"]").unwrap();
    assert_eq!(tab.len(), 1);
    assert_eq!(d.nodes[tab[0]].bounds, (256, 82, 140, 44), "surface (100, 82) + (156, 0)");
    let key = d.select(0, "BUTTON[aria-label=\"Launcher\"]").unwrap();
    assert_eq!(d.nodes[key[0]].bounds, (8, 747, 40, 40), "a panel has no strip");
}

#[test]
fn states_become_attributes() {
    let d = desk();
    let get = |sel: &str| d.nodes[d.select(0, sel).unwrap()[0]].clone();
    assert_eq!(get("[data-testid=\"tab-processes\"]").attr("aria-selected"), Some("true"));
    assert_eq!(get("[data-testid=\"tab-boot\"]").attr("aria-selected"), Some("false"));
    assert_eq!(get("SWITCH").attr("aria-checked"), Some("true"));
    assert_eq!(get("[data-testid=\"sort-name\"]").attr("aria-pressed"), Some("true"));
    assert_eq!(get("[data-testid=\"rack-launcher\"]").attr("aria-pressed"), None, "not pressed, not said");
}

#[test]
fn selectors_match_names_attributes_and_descendants() {
    let d = desk();
    assert_eq!(names(&d, &d.select(0, "TAB").unwrap()), ["Processes", "Boot"]);
    assert_eq!(names(&d, &d.select(0, "tab").unwrap()), ["Processes", "Boot"], "names in any case");
    assert_eq!(names(&d, &d.select(0, "[aria-selected=\"true\"]").unwrap()), ["Processes"]);
    assert_eq!(names(&d, &d.select(0, "TABLIST TAB[aria-label='Boot']").unwrap()), ["Boot"]);
    assert_eq!(names(&d, &d.select(0, "WINDOW[data-owner=\"desktop\"] BUTTON").unwrap()), ["Launcher"]);
    assert!(d.select(0, "WINDOW[data-owner=\"desktop\"] TAB").unwrap().is_empty(), "no tab in the Rack");
    assert_eq!(names(&d, &d.select(0, "[data-testid]").unwrap()).len(), 5);
    // From a node: only what's under it.
    let monitor = d.select(0, "WINDOW[data-owner=\"monitor\"]").unwrap()[0];
    assert_eq!(names(&d, &d.select(monitor, "BUTTON").unwrap()), ["Sort by Service"]);
    for bad in ["", "TAB >", "[aria-label=Boot]", "[x=\"y\"", "a,b", "[=\"v\"]", "[k=\"v\"]TAB"] {
        assert!(d.select(0, bad).is_err() || bad.is_empty(), "{bad:?} should be refused");
    }
    assert!(d.select(0, "").is_err());
}

#[test]
fn a_bad_parent_hangs_from_the_window() {
    let w = [win(9, "Odd", "x", 0, (0, 0, 100, 100), 0)];
    let d = Doc::build(&w, |_| {
        Some(ax::Tree {
            nodes: vec![n(5, ax::BUTTON, "Late", 0, "", (0, 0, 1, 1)), n(0, ax::BUTTON, "Child", 0, "", (0, 0, 1, 1))],
        })
    });
    let late = d.select(0, "[aria-label=\"Late\"]").unwrap()[0];
    assert_eq!(d.nodes[d.nodes[late].parent.unwrap()].name, "WINDOW", "a parent after its child is ignored");
    let child = d.select(0, "[aria-label=\"Child\"]").unwrap()[0];
    assert_eq!(d.nodes[child].parent, Some(late));
}

#[test]
fn the_json_is_cdps() {
    let d = desk();
    let doc = d.node_json(0, -1);
    assert_eq!(doc.get("nodeType").and_then(Value::as_i64), Some(9));
    assert_eq!(doc.get("nodeId").and_then(Value::as_i64), Some(1));
    let kids = doc.get("children").and_then(Value::as_array).unwrap();
    assert_eq!(kids.len(), 2);
    let shallow = d.node_json(0, 1);
    let first = &shallow.get("children").and_then(Value::as_array).unwrap()[0];
    assert!(first.get("children").is_none(), "depth 1 stops at the windows");
    let tab = d.select(0, "[data-testid=\"tab-boot\"]").unwrap()[0];
    let m = d.box_model(tab);
    let content: Vec<i64> = m
        .get("model")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(Value::as_i64)
        .collect();
    assert_eq!(content, [256, 82, 396, 82, 396, 126, 256, 126]);
    let ax = d.ax_tree();
    assert_eq!(ax.get("nodes").and_then(Value::as_array).unwrap().len(), d.nodes.len() - 1);
}

#[test]
fn the_inventory_lists_what_a_person_works() {
    let d = desk();
    let inv = d.inventory();
    let ids: Vec<&str> = inv.iter().filter_map(|v| v.get("label").and_then(Value::as_str)).collect();
    assert_eq!(
        ids,
        ["Launcher", "Twelve-hour clock", "Processes", "Boot", "Sort by Service"],
        "not the tab list or the clock's text"
    );
    assert_eq!(inv[0].get("owner").and_then(Value::as_str), Some("desktop"));
    assert_eq!(inv[2].get("testid").and_then(Value::as_str), Some("tab-processes"));
}

#[test]
fn targets_are_the_desktop_and_each_window() {
    let t = targets(&[win(7, "Monitor", "monitor", 1, (0, 0, 10, 10), 0)]);
    let list = t.get("targetInfos").and_then(Value::as_array).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1].get("targetId").and_then(Value::as_str), Some("7"));
    assert_eq!(list[1].get("url").and_then(Value::as_str), Some("freally://app/monitor"));
}

#[test]
fn the_control_under_a_point_and_the_one_with_the_keyboard() {
    let d = desk();
    let label = |i: Option<usize>| i.map(|i| d.nodes[i].attr("aria-label").unwrap_or("").to_string());
    assert_eq!(label(d.control_at(260, 90)), Some("Boot".into()), "Monitor's Boot tab");
    assert_eq!(label(d.control_at(20, 760)), Some("Launcher".into()), "the Rack's key");
    assert_eq!(label(d.control_at(1150, 770)), Some("Twelve-hour clock".into()));
    assert_eq!(d.control_at(700, 300), None, "Monitor's window, no control there");
    assert_eq!(d.control_at(5, 5), None, "nothing at all");
    // The Rack is under Monitor (z 0): where they overlap, Monitor's tab wins.
    let windows =
        [win(7, "Monitor", "monitor", 1, (0, 700, 400, 100), 0), win(3, "Rack", "desktop", 0, (0, 744, 1280, 56), 0)];
    let d2 = Doc::build(&windows, |id| match id {
        7 => Some(ax::Tree { nodes: vec![n(ax::ROOT, ax::TAB, "Top", 0, "", (0, 0, 400, 100))] }),
        _ => Some(ax::Tree { nodes: vec![n(ax::ROOT, ax::BUTTON, "Under", 0, "", (0, 0, 400, 56))] }),
    });
    assert_eq!(
        d2.control_at(10, 760).map(|i| d2.nodes[i].attr("aria-label").unwrap_or("").to_string()),
        Some("Top".into())
    );
    // The keyboard: the focused window's focused control.
    let w = [win(5, "Launcher", "desktop", 1, (0, 0, 400, 300), 0)];
    let d3 = Doc::build(&w, |_| {
        Some(ax::Tree {
            nodes: vec![n(ax::ROOT, ax::TEXTBOX, "Find an app", ax::FOCUSED, "launcher-search", (0, 0, 10, 10))],
        })
    });
    assert_eq!(
        d3.focused_control().map(|i| d3.nodes[i].attr("data-testid").unwrap_or("").to_string()),
        Some("launcher-search".into())
    );
    assert_eq!(d.focused_control(), None, "Monitor has focus, but no control in it says so");
    let c = d.control(d.control_at(260, 90).unwrap());
    assert_eq!(
        (c.get("owner").and_then(Value::as_str), c.get("role").and_then(Value::as_str)),
        (Some("monitor"), Some("TAB"))
    );
}

/// A stepper: its row (a slider whose box holds its arrows) after its arrow
/// keys in the tree. A press on an arrow works the arrow, not the row: the
/// smallest control under the point is the one pressed.
#[test]
fn the_smallest_control_under_a_point_is_the_one_pressed() {
    let windows = [win(9, "Settings", "settings", 1, (0, 0, 800, 600), 0)];
    let d = Doc::build(&windows, |_| {
        Some(ax::Tree {
            nodes: vec![
                n(ax::ROOT, ax::BUTTON, "Shorter", 0, "settings-idle-down", (316, 104, 32, 32)),
                n(ax::ROOT, ax::BUTTON, "Longer", 0, "settings-idle-up", (360, 104, 32, 32)),
                n(ax::ROOT, ax::SLIDER, "Idle time", 0, "settings-idle", (16, 100, 400, 40)),
            ],
        })
    });
    let id = |i: Option<usize>| i.and_then(|i| d.nodes[i].attr("data-testid")).map(String::from);
    assert_eq!(id(d.control_at(370, 110)), Some("settings-idle-up".into()));
    assert_eq!(id(d.control_at(320, 110)), Some("settings-idle-down".into()));
    assert_eq!(id(d.control_at(100, 110)), Some("settings-idle".into()), "the row itself, away from its arrows");
}

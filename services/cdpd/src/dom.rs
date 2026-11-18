// SPDX-License-Identifier: Apache-2.0
//! The desktop as a document (PRD § 4, *CDP endpoint*): a root, then each
//! window bottom to top, each with the accessibility tree it published under
//! it. A node's `nodeId` is its index plus one; its bounds are on the screen.
//!
//! - Node names are the roles' (`BUTTON`, `SWITCH`, `TAB`, …); attributes
//!   are `data-testid`, `aria-label`, `aria-checked`, `aria-pressed`,
//!   `aria-selected`, `aria-valuenow`, `aria-disabled`, `aria-expanded`.
//! - Selectors: a role name, `[attr]`, `[attr="value"]`, compounds of them,
//!   and the descendant combinator (a space). Nothing else.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use fjson::Value;
use fwire::proto::{ax, stage as st};

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub parent: Option<usize>,
    /// `#document`, `WINDOW`, or the role's name.
    pub name: &'static str,
    pub role: Option<u8>,
    pub attrs: Vec<(String, String)>,
    /// x, y, w, h on the screen.
    pub bounds: (i32, i32, u32, u32),
    pub children: Vec<usize>,
    /// The window it's in (0: the root).
    pub window: u32,
}

impl Node {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Doc {
    pub nodes: Vec<Node>,
}

fn flag(on: bool) -> String {
    String::from(if on { "true" } else { "false" })
}

impl Doc {
    /// The document from stage's windows (bottom to top) and each one's tree.
    pub fn build(windows: &[st::WindowInfo], tree_of: impl Fn(u32) -> Option<ax::Tree>) -> Doc {
        let mut nodes = alloc::vec![Node {
            parent: None,
            name: "#document",
            role: None,
            attrs: Vec::new(),
            bounds: (0, 0, 0, 0),
            children: Vec::new(),
            window: 0,
        }];
        let mut ws: Vec<&st::WindowInfo> = windows.iter().collect();
        ws.sort_by_key(|w| w.z);
        for w in ws {
            let wi = nodes.len();
            nodes[0].children.push(wi);
            let f = &w.frame;
            nodes.push(Node {
                parent: Some(0),
                name: "WINDOW",
                role: None,
                attrs: alloc::vec![
                    (String::from("data-window-id"), w.window_id.to_string()),
                    (String::from("aria-label"), w.title.clone()),
                    (String::from("data-owner"), w.owner.clone()),
                    (String::from("data-kind"), format!("{:?}", w.kind)),
                    (String::from("data-focused"), flag(w.focused)),
                ],
                bounds: (f.x, f.y, f.w, f.h),
                children: Vec::new(),
                window: w.window_id,
            });
            let Some(tree) = tree_of(w.window_id) else { continue };
            let base = nodes.len();
            let s = &w.surface;
            for (i, n) in tree.nodes.iter().enumerate() {
                // A parent must come before its child; anything else hangs
                // from the window.
                let parent =
                    if n.parent != ax::ROOT && (n.parent as usize) < i { base + n.parent as usize } else { wi };
                let me = nodes.len();
                nodes[parent].children.push(me);
                nodes.push(Node {
                    parent: Some(parent),
                    name: ax::NAMES.get(n.role as usize).copied().unwrap_or("GROUP"),
                    role: Some(n.role),
                    attrs: attrs(n),
                    bounds: (s.x + n.x, s.y + n.y, n.w, n.h),
                    children: Vec::new(),
                    window: w.window_id,
                });
            }
        }
        Doc { nodes }
    }

    /// The node with `id` (1-based), if any.
    pub fn get(&self, id: i64) -> Option<&Node> {
        usize::try_from(id - 1).ok().and_then(|i| self.nodes.get(i))
    }

    /// Every node under `from` (index) matching `sel`, in document order.
    pub fn select(&self, from: usize, sel: &str) -> Result<Vec<usize>, String> {
        let chain = parse(sel)?;
        let Some((last, before)) = chain.split_last() else { return Err(String::from("an empty selector")) };
        let under = |mut i: usize| {
            while let Some(p) = self.nodes[i].parent {
                if p == from {
                    return true;
                }
                i = p;
            }
            false
        };
        Ok((0..self.nodes.len())
            .filter(|&i| i != from && under(i) && last.matches(&self.nodes[i]) && self.ancestors_match(i, from, before))
            .collect())
    }

    /// Whether `before` (outermost first) match ancestors of `i`, in order,
    /// inside `from`.
    fn ancestors_match(&self, i: usize, from: usize, before: &[Compound]) -> bool {
        let mut want = before.iter().rev();
        let mut next = want.next();
        let mut at = self.nodes[i].parent;
        while let (Some(c), Some(a)) = (next, at) {
            if a == from {
                break;
            }
            if c.matches(&self.nodes[a]) {
                next = want.next();
            }
            at = self.nodes[a].parent;
        }
        next.is_none()
    }

    /// CDP's `DOM.Node` for index `i`, its children `depth` deep (-1: all).
    pub fn node_json(&self, i: usize, depth: i64) -> Value {
        let n = &self.nodes[i];
        let mut attrs = Vec::new();
        for (k, v) in &n.attrs {
            attrs.push(Value::from(k.as_str()));
            attrs.push(Value::from(v.as_str()));
        }
        let mut o = Value::obj([
            ("nodeId", Value::from(i as u32 + 1)),
            ("backendNodeId", Value::from(i as u32 + 1)),
            ("nodeType", Value::from(if i == 0 { 9u32 } else { 1 })),
            ("nodeName", Value::from(n.name)),
            ("localName", Value::from(n.name.to_ascii_lowercase())),
            ("nodeValue", Value::from("")),
            ("childNodeCount", Value::from(n.children.len() as u32)),
            ("attributes", Value::Arr(attrs)),
        ]);
        if depth != 0 {
            let kids = n.children.iter().map(|&c| self.node_json(c, depth - 1)).collect();
            o.set("children", Value::Arr(kids));
        }
        o
    }

    /// CDP's box model: the node's quad on the screen.
    pub fn box_model(&self, i: usize) -> Value {
        let (x, y, w, h) = self.nodes[i].bounds;
        let (x1, y1) = (x + w as i32, y + h as i32);
        let quad = || Value::Arr([x, y, x1, y, x1, y1, x, y1].iter().map(|&v| Value::from(v)).collect());
        Value::obj([(
            "model",
            Value::obj([
                ("content", quad()),
                ("padding", quad()),
                ("border", quad()),
                ("margin", quad()),
                ("width", Value::from(w)),
                ("height", Value::from(h)),
            ]),
        )])
    }

    /// `Accessibility.getFullAXTree`: every node of every tree.
    pub fn ax_tree(&self) -> Value {
        let nodes = (1..self.nodes.len())
            .map(|i| {
                let n = &self.nodes[i];
                Value::obj([
                    ("nodeId", Value::from((i + 1).to_string())),
                    ("ignored", Value::from(false)),
                    ("role", Value::obj([("type", "role".into()), ("value", n.name.to_ascii_lowercase().into())])),
                    (
                        "name",
                        Value::obj([
                            ("type", "computedString".into()),
                            ("value", n.attr("aria-label").unwrap_or("").into()),
                        ]),
                    ),
                    ("childIds", Value::Arr(n.children.iter().map(|c| Value::from((c + 1).to_string())).collect())),
                    ("backendDOMNodeId", Value::from(i as u32 + 1)),
                ])
            })
            .collect();
        Value::obj([("nodes", Value::Arr(nodes))])
    }

    /// The interactive nodes (the inventory's half from the trees): each with
    /// its window's owner, role, name and test id.
    pub fn inventory(&self) -> Vec<Value> {
        (0..self.nodes.len())
            .filter(|&i| self.nodes[i].role.is_some_and(ax::interactive))
            .map(|i| self.control(i))
            .collect()
    }

    /// One control as the inventory names it.
    pub fn control(&self, i: usize) -> Value {
        let n = &self.nodes[i];
        let owner = self
            .nodes
            .iter()
            .find(|w| w.name == "WINDOW" && w.window == n.window)
            .and_then(|w| w.attr("data-owner"))
            .unwrap_or("");
        Value::obj([
            ("nodeId", Value::from(i as u32 + 1)),
            ("owner", owner.into()),
            ("role", n.name.into()),
            ("label", n.attr("aria-label").unwrap_or("").into()),
            ("testid", n.attr("data-testid").unwrap_or("").into()),
        ])
    }

    /// The control under (x, y) on the screen: in the topmost window there,
    /// the deepest interactive node holding the point.
    pub fn control_at(&self, x: i32, y: i32) -> Option<usize> {
        let inside = |b: (i32, i32, u32, u32)| x >= b.0 && y >= b.1 && x < b.0 + b.2 as i32 && y < b.1 + b.3 as i32;
        let top = self.nodes[0].children.iter().rev().copied().find(|&w| inside(self.nodes[w].bounds))?;
        let window = self.nodes[top].window;
        (0..self.nodes.len()).rev().find(|&i| {
            self.nodes[i].window == window
                && self.nodes[i].role.is_some_and(ax::interactive)
                && inside(self.nodes[i].bounds)
        })
    }

    /// The control with the keyboard: in the focused window, the
    /// interactive node that says it's focused.
    pub fn focused_control(&self) -> Option<usize> {
        let w = self.nodes[0].children.iter().copied().find(|&w| self.nodes[w].attr("data-focused") == Some("true"))?;
        let window = self.nodes[w].window;
        (0..self.nodes.len()).find(|&i| {
            self.nodes[i].window == window
                && self.nodes[i].role.is_some_and(ax::interactive)
                && self.nodes[i].attr("data-focused") == Some("true")
        })
    }
}

fn attrs(n: &ax::Node) -> Vec<(String, String)> {
    let mut a = Vec::new();
    let mut put = |k: &str, v: String| a.push((String::from(k), v));
    if !n.testid.is_empty() {
        put("data-testid", n.testid.clone());
    }
    if !n.name.is_empty() {
        put("aria-label", n.name.clone());
    }
    match n.role {
        ax::SWITCH => put("aria-checked", flag(n.state & ax::CHECKED != 0)),
        ax::TAB | ax::OPTION => put("aria-selected", flag(n.state & ax::SELECTED != 0)),
        _ if n.state & ax::CHECKED != 0 => put("aria-checked", flag(true)),
        _ => {}
    }
    if n.state & ax::PRESSED != 0 {
        put("aria-pressed", flag(true));
    }
    if !n.value.is_empty() {
        put("aria-valuenow", n.value.clone());
    }
    if n.state & ax::DISABLED != 0 {
        put("aria-disabled", flag(true));
    }
    if n.state & ax::EXPANDED != 0 {
        put("aria-expanded", flag(true));
    }
    if n.state & ax::FOCUSED != 0 {
        put("data-focused", flag(true));
    }
    a
}

/// One compound: a node name (or none), and attribute tests.
#[derive(Debug, Default, PartialEq)]
struct Compound {
    name: Option<String>,
    attrs: Vec<(String, Option<String>)>,
}

impl Compound {
    fn matches(&self, n: &Node) -> bool {
        self.name.as_ref().is_none_or(|w| w == "*" || w.eq_ignore_ascii_case(n.name))
            && self.attrs.iter().all(|(k, v)| match (n.attr(k), v) {
                (Some(have), Some(want)) => have == want,
                (Some(_), None) => true,
                (None, _) => false,
            })
    }
}

fn parse(sel: &str) -> Result<Vec<Compound>, String> {
    let bad = || format!("a selector cdpd doesn't read: {sel:?}");
    let mut out: Vec<Compound> = Vec::new();
    let mut cur = Compound::default();
    let mut started = false;
    let mut chars = sel.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if started {
                    out.push(core::mem::take(&mut cur));
                    started = false;
                }
            }
            '[' => {
                let mut key = String::new();
                let mut val: Option<String> = None;
                loop {
                    match chars.next().ok_or_else(bad)? {
                        ']' => break,
                        '=' => {
                            let q = chars.next().ok_or_else(bad)?;
                            if q != '"' && q != '\'' {
                                return Err(bad());
                            }
                            let mut v = String::new();
                            loop {
                                match chars.next().ok_or_else(bad)? {
                                    c if c == q => break,
                                    c => v.push(c),
                                }
                            }
                            val = Some(v);
                            if chars.next() != Some(']') {
                                return Err(bad());
                            }
                            break;
                        }
                        c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => key.push(c),
                        _ => return Err(bad()),
                    }
                }
                if key.is_empty() {
                    return Err(bad());
                }
                cur.attrs.push((key, val));
                started = true;
            }
            c if c.is_ascii_alphanumeric() || c == '*' || c == '-' || c == '#' => {
                if !cur.attrs.is_empty() {
                    return Err(bad());
                }
                cur.name.get_or_insert_with(String::new).push(c);
                started = true;
            }
            _ => return Err(bad()),
        }
    }
    if started {
        out.push(cur);
    }
    Ok(out)
}

/// `Target.getTargets`: the desktop, then each window.
pub fn targets(windows: &[st::WindowInfo]) -> Value {
    let mut list = alloc::vec![Value::obj([
        ("targetId", "desktop".into()),
        ("type", "page".into()),
        ("title", "Freally OS".into()),
        ("url", "freally://desktop".into()),
    ])];
    for w in windows {
        list.push(Value::obj([
            ("targetId", w.window_id.to_string().into()),
            ("type", "page".into()),
            ("title", w.title.as_str().into()),
            ("url", format!("freally://app/{}", w.owner).into()),
        ]));
    }
    Value::obj([("targetInfos", Value::Arr(list))])
}

#[cfg(test)]
mod tests;

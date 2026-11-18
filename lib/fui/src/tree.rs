// SPDX-License-Identifier: Apache-2.0
//! The retained widget tree (TASK-055): nodes in an arena, each with its
//! layout, style, content and accessibility facts. Built once, laid out and
//! painted as often as the theme, scale or size changes.

use crate::a11y::{A11y, Role};
use crate::layout::Rect;
use crate::theme::{TextStyle, Tok};
use alloc::string::String;
use alloc::vec::Vec;

pub type NodeId = usize;

/// A node's size along one axis, in 100 % pixels (the theme scales them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Fixed(u32),
    /// A share of what's left, by weight.
    Fill(u16),
    /// As big as its content.
    Fit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
}

/// How a node arranges its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Row,
    Column,
    Grid {
        columns: u16,
    },
    /// Every child over the same area.
    Stack,
    /// A column taller than its view, moved by `scroll_y`.
    Scroll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub width: Size,
    pub height: Size,
    pub padding: u32,
    pub gap: u32,
    pub align: Align,
    pub background: Option<Tok>,
    pub border: Option<Tok>,
}

impl Default for Style {
    fn default() -> Style {
        Style {
            width: Size::Fit,
            height: Size::Fit,
            padding: 0,
            gap: 0,
            align: Align::Start,
            background: None,
            border: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    None,
    /// Words from the string catalog, by key.
    Text {
        key: String,
        style: TextStyle,
        colour: Tok,
    },
}

#[derive(Clone, Debug)]
pub struct Node {
    pub layout: Layout,
    pub style: Style,
    pub content: Content,
    pub children: Vec<NodeId>,
    pub a11y: A11y,
    /// Where layout put it.
    pub bounds: Rect,
    pub scroll_y: i32,
    /// Its natural size (measure's answer), in device pixels.
    pub natural: (u32, u32),
}

impl Node {
    pub fn new(layout: Layout, role: Role) -> Node {
        Node {
            layout,
            style: Style::default(),
            content: Content::None,
            children: Vec::new(),
            a11y: A11y::new(role),
            bounds: Rect::default(),
            scroll_y: 0,
            natural: (0, 0),
        }
    }

    /// A line of words from the catalog.
    pub fn text(key: &str, style: TextStyle, colour: Tok) -> Node {
        let mut n = Node::new(Layout::Stack, Role::Text);
        n.content = Content::Text { key: String::from(key), style, colour };
        n
    }

    pub fn width(mut self, s: Size) -> Node {
        self.style.width = s;
        self
    }

    pub fn height(mut self, s: Size) -> Node {
        self.style.height = s;
        self
    }

    pub fn padding(mut self, p: u32) -> Node {
        self.style.padding = p;
        self
    }

    pub fn gap(mut self, g: u32) -> Node {
        self.style.gap = g;
        self
    }

    pub fn align(mut self, a: Align) -> Node {
        self.style.align = a;
        self
    }

    pub fn background(mut self, t: Tok) -> Node {
        self.style.background = Some(t);
        self
    }

    pub fn border(mut self, t: Tok) -> Node {
        self.style.border = Some(t);
        self
    }

    /// The accessible name (words from the catalog, already looked up).
    pub fn name(mut self, words: String) -> Node {
        self.a11y.name = Some(words);
        self
    }

    pub fn testid(mut self, id: &str) -> Node {
        self.a11y.testid = Some(String::from(id));
        self
    }
}

#[derive(Debug)]
pub struct Tree {
    pub nodes: Vec<Node>,
}

impl Tree {
    /// A tree whose root (node 0) is `root`.
    pub fn new(root: Node) -> Tree {
        Tree { nodes: alloc::vec![root] }
    }

    pub const ROOT: NodeId = 0;

    pub fn add(&mut self, parent: NodeId, node: Node) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(node);
        self.nodes[parent].children.push(id);
        id
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id]
    }

    pub fn find(&self, testid: &str) -> Option<NodeId> {
        self.nodes.iter().position(|n| n.a11y.testid.as_deref() == Some(testid))
    }

    /// What's wrong with the tree's accessibility: every node that isn't
    /// only structure needs a name (text nodes get theirs from their words
    /// at layout), and test ids are unique.
    pub fn check(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (i, n) in self.nodes.iter().enumerate() {
            let named =
                n.a11y.name.as_deref().is_some_and(|s| !s.is_empty()) || matches!(n.content, Content::Text { .. });
            if !n.a11y.role.is_structure() && !named {
                problems.push(alloc::format!("node {i} ({:?}) has no name", n.a11y.role));
            }
            if let Some(t) = &n.a11y.testid
                && self.nodes[..i].iter().any(|m| m.a11y.testid.as_ref() == Some(t))
            {
                problems.push(alloc::format!("test id {t} twice"));
            }
        }
        problems
    }
}

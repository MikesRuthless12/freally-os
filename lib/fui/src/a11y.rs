// SPDX-License-Identifier: Apache-2.0
//! The accessibility tree (TASK-055, FR-027): every node says what it is,
//! what it's called, its state, its value, where it is, and its test id.
//! Stage publishes it after each layout; cdpd reads it as the DOM (TASK-063).
//! A node that can be seen and has no name is a bug: `Tree::check` finds it.

use crate::layout::Rect;
use alloc::string::String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Window,
    Group,
    Text,
    Button,
    Switch,
    Slider,
    Knob,
    Meter,
    TextField,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    TabList,
    Tab,
    Menu,
    MenuItem,
    Dialog,
    Tooltip,
    Image,
    ScrollArea,
}

impl Role {
    /// Roles that are only structure: they needn't be named.
    pub fn is_structure(self) -> bool {
        matches!(self, Role::Group | Role::Row | Role::ScrollArea)
    }
}

/// State bits.
pub mod state {
    pub const FOCUSED: u32 = 1 << 0;
    pub const PRESSED: u32 = 1 << 1;
    pub const CHECKED: u32 = 1 << 2;
    pub const DISABLED: u32 = 1 << 3;
    pub const EXPANDED: u32 = 1 << 4;
    pub const SELECTED: u32 = 1 << 5;
    pub const HOVERED: u32 = 1 << 6;
}

#[derive(Clone, Debug, PartialEq)]
pub struct A11y {
    pub role: Role,
    /// What a screen reader says (a string catalog's words, never a key).
    pub name: Option<String>,
    pub state: u32,
    pub value: Option<String>,
    pub testid: Option<String>,
    /// Filled in by layout.
    pub bounds: Rect,
}

impl A11y {
    pub fn new(role: Role) -> A11y {
        A11y { role, name: None, state: 0, value: None, testid: None, bounds: Rect::default() }
    }
}

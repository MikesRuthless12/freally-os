// SPDX-License-Identifier: Apache-2.0
//! The Brain's tools and resources (PRD § 4; the list signed off by the
//! owner on 2026-11-18): seven to read the system, six to act on it. Each
//! input schema is JSON Schema, checked before the tool runs.

/// One tool: its name, a line for the client to show, what it does, and
/// its input schema (JSON).
#[derive(Clone, Copy, Debug)]
pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub schema: &'static str,
    /// It changes the system (and is logged as such).
    pub acts: bool,
}

const EMPTY: &str = r#"{"type":"object","properties":{},"additionalProperties":false}"#;

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "system_status",
        title: "System status",
        description: "Freally OS's version, build, flavour, slot, uptime, boot time, CPUs and memory.",
        schema: EMPTY,
        acts: false,
    },
    ToolDef {
        name: "boot_timeline",
        title: "Boot timeline",
        description: "Every boot stage (firmware, bootloader, kernel, services, desktop) and when it was reached, in ms from the bootloader's start.",
        schema: EMPTY,
        acts: false,
    },
    ToolDef {
        name: "list_processes",
        title: "Processes",
        description: "Every process: its name, what it does, its state, its memory and its CPU use.",
        schema: EMPTY,
        acts: false,
    },
    ToolDef {
        name: "list_services",
        title: "Services",
        description: "Every service as conductor keeps it: state, pid, restarts, binary hash, CPU time and memory.",
        schema: EMPTY,
        acts: false,
    },
    ToolDef {
        name: "read_log",
        title: "Read the log",
        description: "The log's records, oldest first, optionally from one service, at or above a level, after a sequence number.",
        schema: r#"{"type":"object","properties":{"service":{"type":"string","maxLength":64},"min_level":{"enum":["trace","debug","info","warn","error"]},"since_seq":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":1000}},"additionalProperties":false}"#,
        acts: false,
    },
    ToolDef {
        name: "get_faults",
        title: "Faults",
        description: "Every fault report (crashes, panics), optionally after a sequence number.",
        schema: r#"{"type":"object","properties":{"since_seq":{"type":"integer","minimum":0}},"additionalProperties":false}"#,
        acts: false,
    },
    ToolDef {
        name: "screenshot",
        title: "Screenshot",
        description: "A PNG of the whole screen, or of one window by its title, as it shows now (the fleet cursor included).",
        schema: r#"{"type":"object","properties":{"window":{"type":"string","maxLength":128}},"additionalProperties":false}"#,
        acts: false,
    },
    ToolDef {
        name: "send_input",
        title: "Send input",
        description: "Types text, presses a key chord (\"Super+M\") or clicks at a point, as a person at the keyboard would, into whatever has focus.",
        schema: r#"{"type":"object","properties":{"text":{"type":"string","maxLength":1000},"keys":{"type":"string","maxLength":64},"click":{"type":"object","properties":{"x":{"type":"integer","minimum":0},"y":{"type":"integer","minimum":0},"button":{"enum":["left","right","middle"]}},"required":["x","y"],"additionalProperties":false}},"additionalProperties":false}"#,
        acts: true,
    },
    ToolDef {
        name: "restart_service",
        title: "Restart a service",
        description: "Stops a service and starts it again.",
        schema: r#"{"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":64}},"required":["name"],"additionalProperties":false}"#,
        acts: true,
    },
    ToolDef {
        name: "swap_service",
        title: "Swap a running service",
        description: "Replaces a running service with a new build of it, live, handing its state over; refused unless the build is signed with the dev key (cargo xtask brain-swap makes the arguments).",
        schema: r#"{"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":64},"binary_base64":{"type":"string","minLength":4},"signature_base64":{"type":"string","minLength":4}},"required":["name","binary_base64","signature_base64"],"additionalProperties":false}"#,
        acts: true,
    },
    ToolDef {
        name: "rollback_service",
        title: "Roll a swap back",
        description: "Puts back the build a service ran before its last swap.",
        schema: r#"{"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":64}},"required":["name"],"additionalProperties":false}"#,
        acts: true,
    },
    ToolDef {
        name: "set_theme",
        title: "Set the theme",
        description: "Dark or light, everywhere, at once.",
        schema: r#"{"type":"object","properties":{"theme":{"enum":["dark","light"]}},"required":["theme"],"additionalProperties":false}"#,
        acts: true,
    },
    ToolDef {
        name: "reboot",
        title: "Reboot",
        description: "Restarts the machine, into the same slot or the other one; the connection drops.",
        schema: r#"{"type":"object","properties":{"slot":{"enum":["same","other"]}},"additionalProperties":false}"#,
        acts: true,
    },
];

/// The tool called `name`.
pub fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

/// One resource: its URI, name and type.
#[derive(Clone, Copy, Debug)]
pub struct ResourceDef {
    pub uri: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime: &'static str,
}

/// The fixed resources; each service's manifest is one more
/// (`freally://services/<name>/manifest`, [`MANIFEST_TEMPLATE`]).
pub const RESOURCES: &[ResourceDef] = &[
    ResourceDef {
        uri: "freally://log/live",
        name: "The log, live",
        description: "The newest log records; subscribe to be told when more arrive.",
        mime: "text/plain",
    },
    ResourceDef {
        uri: "freally://boot/timeline",
        name: "The boot timeline",
        description: "Each boot stage and when it was reached.",
        mime: "application/json",
    },
];

/// Each service's manifest, as conductor read it.
pub const MANIFEST_TEMPLATE: &str = "freally://services/{name}/manifest";

/// The resource that can be subscribed to.
pub const LOG_LIVE: &str = "freally://log/live";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signed_off_list_is_whole_and_every_schema_parses() {
        let names: alloc::vec::Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "system_status",
                "boot_timeline",
                "list_processes",
                "list_services",
                "read_log",
                "get_faults",
                "screenshot",
                "send_input",
                "restart_service",
                "swap_service",
                "rollback_service",
                "set_theme",
                "reboot"
            ]
        );
        for t in TOOLS {
            let s = fjson::parse(t.schema).unwrap_or_else(|e| panic!("{}: {e:?}", t.name));
            assert_eq!(s.get("type").and_then(fjson::Value::as_str), Some("object"), "{}", t.name);
        }
        assert_eq!(TOOLS.iter().filter(|t| t.acts).count(), 6);
    }
}

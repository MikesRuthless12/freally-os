// SPDX-License-Identifier: Apache-2.0
//! Settings ▸ About (FR-048): the version, the slot it started from, and
//! the licences: Apache-2.0 for the code, the brand terms for the name and
//! the mark, and the third-party notices (Rust's own libraries, the Noto
//! fonts, Unicode's data).

use alloc::string::String;
use alloc::vec::Vec;
use fstd::machine::Image;

/// The About panel's lines: (label, words, test id).
pub fn lines(image: Option<&Image>) -> Vec<(&'static str, String, &'static str)> {
    let flavour = |f: u8| match f {
        0 => "",
        1 => " (a developer build)",
        _ => " (a test build)",
    };
    let (version, slot) = match image {
        Some(i) => (
            alloc::format!("{}{}", i.version, flavour(i.flavour)),
            alloc::format!("{}, build {}", i.slot_name(), &i.build_hex()[..12]),
        ),
        None => (String::from(fcore_abi::VERSION), String::from("unknown")),
    };
    alloc::vec![
        ("Version", version, "settings-about-version"),
        ("Started from slot", slot, "settings-about-slot"),
        ("Licence", String::from("Apache-2.0: every line of Freally OS is ours."), "settings-about-licence"),
        (
            "The name and the mark",
            String::from("Freally and the green F are the brand's: TRADEMARKS.md says how they may be used."),
            "settings-about-brand",
        ),
        (
            "Third parties",
            String::from("Rust's core libraries, the Noto fonts and Unicode's data: THIRD-PARTY-NOTICES.md."),
            "settings-about-notices",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_and_slot_come_from_the_running_image() {
        let i = Image { slot: 1, flavour: 2, version: String::from("0.7.0"), build_id: [0xAB; 20] };
        let l = lines(Some(&i));
        assert_eq!(l[0].1, "0.7.0 (a test build)");
        assert_eq!(l[1].1, "B, build abababababab");
        assert_eq!(lines(None)[1].1, "unknown");
    }
}

// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask atlas`: renders the boot atlas from the Noto fonts with
//! Freally's own rasterizer (`ffont`), deterministically, into
//! `assets/fonts/noto/boot-atlas.fatl` (OFL-1.1, like the fonts it comes from).
//! `--check` (a CI gate) fails if the committed atlas isn't what the fonts give.

use crate::util;

pub const ATLAS_PATH: &str = "assets/fonts/noto/boot-atlas.fatl";

pub fn render() -> Result<Vec<u8>, String> {
    let root = util::repo_root();
    let read =
        |name: &str| std::fs::read(root.join("assets/fonts/noto").join(name)).map_err(|e| format!("{name}: {e}"));
    let semibold = read("NotoSans-SemiBold.ttf")?;
    let regular = read("NotoSans-Regular.ttf")?;
    let semibold = ffont::ttf::Font::parse(&semibold).map_err(|e| format!("NotoSans-SemiBold.ttf: {e:?}"))?;
    let regular = ffont::ttf::Font::parse(&regular).map_err(|e| format!("NotoSans-Regular.ttf: {e:?}"))?;
    Ok(ffont::atlas::build(&[
        (&semibold, fscreen::TITLE_PX, fscreen::ATLAS_CHARS),
        (&regular, fscreen::TEXT_PX, fscreen::ATLAS_CHARS),
    ]))
}

pub fn atlas_cmd(args: &[String]) -> Result<(), String> {
    let bytes = render()?;
    let path = util::repo_root().join(ATLAS_PATH);
    if args.iter().any(|a| a == "--check") {
        let committed = std::fs::read(&path).map_err(|e| format!("{ATLAS_PATH}: {e}"))?;
        return if committed == bytes {
            println!("atlas: {ATLAS_PATH} is what the fonts give ({} bytes)", bytes.len());
            Ok(())
        } else {
            Err(format!("{ATLAS_PATH} is stale or edited: run cargo xtask atlas"))
        };
    }
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    println!("atlas: {ATLAS_PATH} written ({} bytes)", bytes.len());
    Ok(())
}

// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask screen title|integrity [--size WxH] --out file.png`: draws a
//! pre-desktop screen on the host, exactly as the bootloader does, to look at it.

pub fn to_png(w: usize, h: usize, px: &[u32]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(w * h * 3);
    for p in px {
        rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
    }
    fimage::png::encode(w as u32, h as u32, 3, &rgb)
}

pub fn screen_cmd(args: &[String]) -> Result<(), String> {
    let which = args.first().map(String::as_str).ok_or("cargo xtask screen title|integrity --out file.png")?;
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let (w, h) = match arg("--size") {
        Some(s) => {
            let (a, b) = s.split_once('x').ok_or("--size WxH")?;
            (a.parse().map_err(|_| "bad width")?, b.parse().map_err(|_| "bad height")?)
        }
        None => (1280usize, 800usize),
    };
    let out = arg("--out").ok_or("--out file.png")?;
    let mut px = vec![0u32; w * h];
    let mut c = fscreen::Canvas { px: &mut px, w, h };
    let r = match which {
        "title" => fscreen::title(&mut c, fcore_abi::VERSION, "Every block checked. Signed by the dev key."),
        "integrity" => fscreen::integrity(&mut c, fsi::FsiError::BadBlock(3).describe()),
        _ => return Err(format!("unknown screen {which:?}")),
    };
    std::fs::write(&out, to_png(w, h, &px)).map_err(|e| e.to_string())?;
    println!("screen: {which} → {out} (title box {r:?})");
    Ok(())
}

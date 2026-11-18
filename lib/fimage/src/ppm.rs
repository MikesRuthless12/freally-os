// SPDX-License-Identifier: Apache-2.0
//! Binary PPM (P6, maxval 255): the format QEMU's `screendump` writes.

/// `(width, height, rgb)` borrowed from a P6 file.
pub fn parse(data: &[u8]) -> Result<(u32, u32, &[u8]), &'static str> {
    if data.get(..2) != Some(b"P6") {
        return Err("not a P6 PPM");
    }
    let mut p = 2;
    let mut fields = [0u32; 3];
    for f in &mut fields {
        // Skip whitespace and comments.
        loop {
            match data.get(p) {
                Some(b'#') => {
                    while data.get(p).is_some_and(|&c| c != b'\n') {
                        p += 1;
                    }
                }
                Some(c) if c.is_ascii_whitespace() => p += 1,
                Some(_) => break,
                None => return Err("truncated header"),
            }
        }
        let start = p;
        while data.get(p).is_some_and(|c| c.is_ascii_digit()) {
            p += 1;
        }
        let digits = core::str::from_utf8(&data[start..p]).map_err(|_| "bad number")?;
        *f = digits.parse().map_err(|_| "bad number")?;
    }
    if fields[2] != 255 {
        return Err("only maxval 255");
    }
    p += 1; // the single whitespace byte after maxval
    let (w, h) = (fields[0], fields[1]);
    let len = (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(3)).ok_or("too large")?;
    let rgb = data.get(p..p + len).ok_or("truncated pixels")?;
    Ok((w, h, rgb))
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_a_small_ppm_with_a_comment() {
        let mut f = b"P6\n# made by QEMU\n2 1\n255\n".to_vec();
        f.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let (w, h, rgb) = super::parse(&f).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgb, [1, 2, 3, 4, 5, 6]);
        assert!(super::parse(&f[..f.len() - 1]).is_err());
    }
}

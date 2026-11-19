// SPDX-License-Identifier: Apache-2.0
//! CRC-32C (Castagnoli: 0x1EDC6F41, reflected as 0x82F63B78; initial and
//! final XOR all ones), the checksum of every plain node and superblock.

const TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0x82F6_3B78 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

pub fn crc32c(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |c, &b| TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_check_value() {
        // RFC 3720 B.4 and the CRC catalogue: "123456789" → E3069283.
        assert_eq!(super::crc32c(b"123456789"), 0xE306_9283);
        // 32 zero bytes (RFC 3720 B.4): 8A9136AA.
        assert_eq!(super::crc32c(&[0u8; 32]), 0x8A91_36AA);
    }
}

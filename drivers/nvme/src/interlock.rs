// SPDX-License-Identifier: Apache-2.0
//! The partition interlock (TASK-092, PRD § 9; charter rule 6: never touch a
//! disk you don't own). Before any write reaches the controller, the run of
//! blocks must lie wholly inside one partition of a Freally type
//! (`FREALLY_SYSTEM` or `FREALLY_DATA`) of the disk's own GPT, read when the
//! driver starts. Anything else is `AccessDenied`: the ESP, the GPT itself,
//! another system's partitions, a disk with no Freally partition at all
//! (read-only), a run that crosses a partition's end. Reads are never
//! refused. `cargo xtask validate` lets an image carry this driver only
//! while this file's tests are here (TASK-077's no-writes rule).

use fcore_abi::FErr;

/// What the driver knows of its disk's partitions: each Freally partition's
/// first and last (inclusive) block. Empty: the disk is read-only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Interlock {
    owned: alloc::vec::Vec<(u64, u64)>,
}

impl Interlock {
    /// From the disk's checked GPT ([`fgpt::Table`]): its Freally partitions.
    pub fn from_table(t: &fgpt::Table) -> Interlock {
        let owned = t
            .partitions()
            .filter(|p| p.type_guid == fgpt::PART_TYPE_FREALLY_SYSTEM || p.type_guid == fgpt::PART_TYPE_FREALLY_DATA)
            .map(|p| (p.first_lba, p.last_lba))
            .collect();
        Interlock { owned }
    }

    /// A disk with no GPT, or one the driver couldn't read: nothing may be written.
    pub fn read_only() -> Interlock {
        Interlock::default()
    }

    /// Whether `count` blocks from `lba` may be written: `Ok` only inside one
    /// Freally partition.
    pub fn check_write(&self, lba: u64, count: u64) -> Result<(), FErr> {
        let Some(end) = lba.checked_add(count) else { return Err(FErr::AccessDenied) };
        if count == 0 || !self.owned.iter().any(|&(first, last)| lba >= first && end <= last + 1) {
            return Err(FErr::AccessDenied);
        }
        Ok(())
    }

    /// The disk can't be written anywhere.
    pub fn is_read_only(&self) -> bool {
        self.owned.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fgpt::build::{Part, guid_from, tables};
    use fgpt::{Header, PART_TYPE_ESP, Table};

    const BLOCKS: u64 = 4 << 21; // 4 GiB of 512-byte blocks

    /// A disk with Freally OS's layout, plus another system's partition at
    /// the end (where FREALLY_DATA would reach in a real layout).
    fn disk(freally: bool) -> Interlock {
        let l = fgpt::freally_layout(BLOCKS, 512).unwrap();
        let mut parts: Vec<Part> = l
            .iter()
            .enumerate()
            .filter(|(i, _)| freally || *i == 0)
            .map(|(i, &(type_guid, name, first_lba, last_lba))| Part {
                type_guid,
                unique: guid_from([i as u8; 16]),
                first_lba,
                last_lba: if i == 3 { last_lba - 4096 } else { last_lba },
                name,
            })
            .collect();
        // Windows' basic data partition after the data.
        let windows =
            fcore_abi::Guid::new(0xEBD0_A0A2, 0xB9E5, 0x4433, [0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7]);
        parts.push(Part {
            type_guid: windows,
            unique: guid_from([9; 16]),
            first_lba: l[3].3 - 4095,
            last_lba: l[3].3,
            name: "Windows",
        });
        let (start, _) = tables(BLOCKS, 512, guid_from([7; 16]), &parts);
        let h = Header::parse(&start[512..1024], 1, BLOCKS - 1).unwrap();
        let t = Table::new(h, &start[1024..]).unwrap();
        assert_eq!(t.partitions().filter(|p| p.type_guid == PART_TYPE_ESP).count(), 1);
        Interlock::from_table(&t)
    }

    #[test]
    fn a_write_outside_the_freally_partitions_is_refused() {
        let i = disk(true);
        let l = fgpt::freally_layout(BLOCKS, 512).unwrap();
        let (esp, a, data) = (l[0], l[1], l[3]);
        // Inside: allowed.
        assert_eq!(i.check_write(data.2, 8), Ok(()));
        assert_eq!(i.check_write(a.2, 1024), Ok(()));
        assert_eq!(i.check_write(data.3 - 4096, 1), Ok(()), "FREALLY_DATA's last block");
        // Outside: AccessDenied, every way.
        for (lba, n, what) in [
            (0, 1, "the protective MBR"),
            (1, 33, "the GPT"),
            (esp.2, 1, "the ESP"),
            (data.3 - 4095, 1, "another system's partition"),
            (data.3 - 4096, 2, "across FREALLY_DATA's end into Windows'"),
            (a.3, 2, "across two partitions"),
            (BLOCKS - 1, 1, "the backup GPT"),
            (data.2, 0, "nothing"),
            (u64::MAX - 1, 4, "a run that wraps"),
        ] {
            assert_eq!(i.check_write(lba, n), Err(FErr::AccessDenied), "{what}");
        }
    }

    #[test]
    fn a_disk_without_freally_partitions_is_read_only() {
        let i = disk(false);
        assert!(i.is_read_only());
        assert_eq!(i.check_write(4096, 1), Err(FErr::AccessDenied));
        assert_eq!(Interlock::read_only().check_write(0, 1), Err(FErr::AccessDenied));
    }
}

// SPDX-License-Identifier: Apache-2.0
//! The log ring: records in arrival order, the oldest dropped once the total
//! passes the budget (4 MiB in the service). Reading pages forward by
//! sequence number.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use fwire::proto::logger::{Filter, LogRecord, Records};

pub struct Ring {
    records: VecDeque<LogRecord>,
    bytes: usize,
    budget: usize,
    next_seq: u64,
}

/// What a record costs against the budget.
fn cost(r: &LogRecord) -> usize {
    24 + r.svc.len() + r.msg.len()
}

/// A reply page holds at most this much (a channel message is 64 KiB).
const PAGE_BYTES: usize = 48 * 1024;

impl Ring {
    pub fn new(budget: usize) -> Ring {
        Ring { records: VecDeque::new(), bytes: 0, budget, next_seq: 1 }
    }

    pub fn push(&mut self, ts_ns: u64, level: u8, svc: &str, msg: &str) -> u64 {
        let r = LogRecord { seq: self.next_seq, ts_ns, level, svc: String::from(svc), msg: String::from(msg) };
        self.next_seq += 1;
        self.bytes += cost(&r);
        self.records.push_back(r);
        while self.bytes > self.budget {
            match self.records.pop_front() {
                Some(old) => self.bytes -= cost(&old),
                None => break,
            }
        }
        self.next_seq - 1
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Records with `seq >= cursor` that pass `filter`, oldest first, at most
    /// `limit` and one page of bytes; `next_cursor` 0 when nothing is left.
    pub fn read(&self, cursor: u64, limit: u16, filter: &Filter) -> Records {
        let pass = |r: &&LogRecord| {
            r.seq >= cursor
                && filter.svc.as_ref().is_none_or(|s| *s == r.svc)
                && filter.min_level.is_none_or(|l| r.level >= l)
        };
        let mut out: Vec<LogRecord> = Vec::new();
        let mut size = 0;
        let mut more = false;
        for r in self.records.iter().filter(pass) {
            if out.len() == limit as usize || size + cost(r) > PAGE_BYTES {
                more = true;
                break;
            }
            size += cost(r);
            out.push(r.clone());
        }
        let next_cursor = if more { out.last().map_or(0, |r| r.seq + 1) } else { 0 };
        Records { records: out, next_cursor, newest: self.records.back().map_or(0, |r| r.seq) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Filter {
        Filter { svc: None, min_level: None }
    }

    #[test]
    fn the_oldest_records_go_first_when_the_budget_is_spent() {
        let mut r = Ring::new(1000);
        for i in 0..100u64 {
            r.push(i, 2, "svc", "a message of thirty characters");
        }
        // Each record costs 24 + 3 + 30 = 57 bytes: 17 fit in 1,000.
        assert_eq!(r.len(), 17);
        let page = r.read(0, 100, &all());
        assert_eq!(page.records.first().unwrap().seq, 84);
        assert_eq!(page.records.last().unwrap().seq, 100);
        assert_eq!(page.next_cursor, 0);
    }

    #[test]
    fn reading_pages_forward_and_filters() {
        let mut r = Ring::new(1 << 20);
        for i in 0..10u64 {
            r.push(i, if i % 2 == 0 { 2 } else { 4 }, if i < 5 { "a" } else { "b" }, "m");
        }
        let p1 = r.read(0, 4, &all());
        assert_eq!(p1.records.iter().map(|x| x.seq).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(p1.next_cursor, 5);
        let p2 = r.read(p1.next_cursor, 100, &all());
        assert_eq!(p2.records.len(), 6);
        assert_eq!(p2.next_cursor, 0);
        let b = r.read(0, 100, &Filter { svc: Some(String::from("b")), min_level: None });
        assert!(b.records.iter().all(|x| x.svc == "b") && b.records.len() == 5);
        let errors = r.read(0, 100, &Filter { svc: None, min_level: Some(4) });
        assert!(errors.records.iter().all(|x| x.level == 4) && errors.records.len() == 5);
    }

    #[test]
    fn a_page_never_outgrows_a_message() {
        let mut r = Ring::new(4 << 20);
        let long = "x".repeat(1000);
        for i in 0..200 {
            r.push(i, 2, "svc", &long);
        }
        let p = r.read(0, u16::MAX, &all());
        assert!(fwire::to_payload(&p).len() <= 64 * 1024 - 16);
        assert!(p.next_cursor != 0);
    }
}

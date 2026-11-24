// SPDX-License-Identifier: Apache-2.0
//! The fuzz target for fhttp's reader (TASK-185): responses of each framing,
//! bytes changed, cut or added, fed in pieces of random sizes, then the
//! connection closed. Nothing may panic. `ci --slow` runs 2,000 (`FREALLY_FUZZ_ITERS`).

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[test]
fn changed_responses_never_panic() {
    let iters: u64 = std::env::var("FREALLY_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let seeds: [&[u8]; 4] = [
        b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nX: y\r\n\r\nhello world",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;e=1\r\nhello\r\n6\r\n world\r\n0\r\nT: 1\r\n\r\n",
        b"HTTP/1.0 200 OK\r\n\r\nto the close",
        b"HTTP/1.1 301 Moved\r\nLocation: https://cdn.freally.local/x\r\nContent-Length: 0\r\n\r\n",
    ];
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    for i in 0..iters {
        for s in seeds {
            let mut b = s.to_vec();
            match rng.next() % 3 {
                0 => (0..1 + rng.next() % 6).for_each(|_| {
                    let at = (rng.next() as usize) % b.len();
                    b[at] = rng.next() as u8;
                }),
                1 => b.truncate((rng.next() as usize) % b.len()),
                _ => {
                    let at = (rng.next() as usize) % b.len();
                    let n = 1 + rng.next() as usize % 16;
                    let extra: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                    b.splice(at..at, extra);
                }
            }
            let limit = 1 + (rng.next() % 64) as usize;
            let cuts: Vec<usize> = (0..b.len()).map(|_| 1 + (rng.next() % 7) as usize).collect();
            let run = || {
                let mut r = fhttp::Reader::new(limit);
                let mut at = 0;
                for n in &cuts {
                    if at >= b.len() {
                        break;
                    }
                    let end = (at + n).min(b.len());
                    if r.feed(&b[at..end]).is_err() {
                        return;
                    }
                    at = end;
                }
                if let Ok(resp) = r.close() {
                    let _ = fhttp::redirect(resp);
                }
            };
            if std::panic::catch_unwind(run).is_err() {
                panic!("iteration {i}: the reader panicked on {:?}", String::from_utf8_lossy(&b));
            }
        }
    }
}

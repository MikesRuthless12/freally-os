// SPDX-License-Identifier: Apache-2.0
//! The runner's two ends (TASK-184 part 4): cdp-host, which reaches the
//! machine over QMP (and cdpd through COM3's door), and, when the setup has
//! it, cdpd's own port on the VM's network, where cdpd's methods go
//! straight. cdpd's end opens at its first call and again after a reboot or
//! a dropped connection, waiting while the guest comes up; every method it
//! serves only reads, so a call its connection dropped is sent again.

use cdp_proto::{Cdp, cdpd_serves};
use fjson::Value;
use std::time::{Duration, Instant};

/// How long cdpd's port may take to answer: netstack's address, then cdpd's listen.
const CDPD_WAIT: Duration = Duration::from_secs(60);

pub struct Ends {
    pub host: Cdp,
    /// cdpd's port, and its connection once open.
    cdpd: Option<(String, Option<Cdp>)>,
    wait: Duration,
    /// cdpd's methods by its port, and through cdp-host (COM3's door):
    /// TASK-184 part 5 times the leg on each road.
    by_port: Tally,
    by_door: Tally,
}

/// Calls taken, and the time they took.
#[derive(Default, Clone, Copy)]
struct Tally {
    calls: u64,
    time: Duration,
}

impl Tally {
    fn add(&mut self, start: Instant) {
        self.calls += 1;
        self.time += start.elapsed();
    }

    fn said(self, road: &str) -> String {
        let each = if self.calls == 0 { 0.0 } else { self.time.as_secs_f64() * 1e3 / self.calls as f64 };
        format!("{} {road} in {:.1} s ({each:.1} ms each)", self.calls, self.time.as_secs_f64())
    }
}

impl Ends {
    pub fn new(host: Cdp, cdpd: Option<String>) -> Ends {
        Ends {
            host,
            cdpd: cdpd.map(|a| (a, None)),
            wait: CDPD_WAIT,
            by_port: Tally::default(),
            by_door: Tally::default(),
        }
    }

    /// Calls `method` at the end that serves it; `Err` carries CDP's error,
    /// or says cdpd's port never answered.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if !cdpd_serves(method) {
            return self.host.call(method, params);
        }
        let start = Instant::now();
        match self.cdpd.as_mut() {
            Some((addr, conn)) => {
                let r = by_port(addr, conn, self.wait, method, params);
                self.by_port.add(start);
                r
            }
            None => {
                let r = self.host.call(method, params);
                self.by_door.add(start);
                r
            }
        }
    }

    /// cdpd's methods on each road, for the leg's log.
    pub fn roads(&self) -> String {
        format!(
            "cdpd's methods: {}; {}",
            self.by_port.said("by its port"),
            self.by_door.said("through cdp-host and COM3's door")
        )
    }

    /// Events (`Log.entryAdded`) come from cdp-host.
    pub fn take_events(&mut self) -> Vec<Value> {
        self.host.take_events()
    }

    /// The VM rebooted: cdpd's connection went with it.
    pub fn rebooted(&mut self) {
        if let Some((_, c)) = &mut self.cdpd {
            *c = None;
        }
    }
}

/// A call on cdpd's port: a dropped connection opened again and the call
/// sent again, once.
fn by_port(addr: &str, conn: &mut Option<Cdp>, wait: Duration, method: &str, params: Value) -> Result<Value, String> {
    for _ in 0..2 {
        if conn.is_none() {
            *conn = Some(open(addr, wait)?);
        }
        match conn.as_mut().map(|c| c.exchange(method, params.clone())) {
            Some(Ok(answer)) => return answer,
            _ => *conn = None, // dropped: opened again, the call sent again
        }
    }
    Err(format!("cdpd's port at {addr} dropped {method} twice"))
}

/// cdpd's port, tried until it answers a WebSocket's handshake (QEMU takes
/// the connection at once and drops it while nothing listens in the guest).
fn open(addr: &str, wait: Duration) -> Result<Cdp, String> {
    let start = Instant::now();
    loop {
        match Cdp::connect_ws(addr, "/devtools/page/desktop") {
            Ok(c) => return Ok(c),
            Err(e) if start.elapsed() >= wait => return Err(format!("cdpd's port at {addr} never answered: {e}")),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdp_proto::{Message, WebSocket, read_request};
    use std::io::BufReader;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A WebSocket end answering each call `{"from": name}`, counting its
    /// connections; the first `DOM.flaky` it gets closes the connection unanswered.
    fn end(name: &'static str) -> (String, Arc<AtomicUsize>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let opened = Arc::new(AtomicUsize::new(0));
        let (count, flaky) = (Arc::clone(&opened), Arc::new(AtomicUsize::new(0)));
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                count.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(s.try_clone().unwrap());
                let Ok(req) = read_request(&mut reader) else { continue };
                let Ok(mut ws) = WebSocket::accept(reader, &req) else { continue };
                while let Ok(Message::Text(t)) = ws.recv() {
                    let v = fjson::parse(&t).unwrap();
                    if v.get("method").and_then(Value::as_str) == Some("DOM.flaky")
                        && flaky.fetch_add(1, Ordering::SeqCst) == 0
                    {
                        break;
                    }
                    let id = v.get("id").cloned().unwrap_or(Value::Null);
                    let reply = Value::obj([("id", id), ("result", Value::obj([("from", name.into())]))]);
                    if ws.send_text(&reply.to_json()).is_err() {
                        break;
                    }
                }
            }
        });
        (addr, opened)
    }

    fn from(ends: &mut Ends, method: &str) -> String {
        let r = ends.call(method, Value::Obj(Vec::new())).unwrap();
        r.get("from").and_then(Value::as_str).unwrap().to_string()
    }

    #[test]
    fn cdpds_methods_go_to_its_port_the_machines_to_cdp_host() {
        let ((host, _), (cdpd, opened)) = (end("host"), end("cdpd"));
        let mut e = Ends::new(Cdp::connect_ws(&host, "/").unwrap(), Some(cdpd));
        for m in ["DOM.getDocument", "Accessibility.getFullAXTree", "Log.read", "Freally.readBack"] {
            assert_eq!(from(&mut e, m), "cdpd", "{m}");
        }
        for m in ["Input.dispatchKeyEvent", "Page.captureScreenshot", "Page.reload", "Log.enable", "Freally.qemuPci"] {
            assert_eq!(from(&mut e, m), "host", "{m}");
        }
        assert_eq!(opened.load(Ordering::SeqCst), 1, "one connection, kept");
        // A dropped connection: opened again, the call sent again.
        assert_eq!(from(&mut e, "DOM.flaky"), "cdpd");
        assert_eq!(opened.load(Ordering::SeqCst), 2);
        // A reboot: the next call opens it again.
        e.rebooted();
        assert_eq!(from(&mut e, "DOM.getDocument"), "cdpd");
        assert_eq!(opened.load(Ordering::SeqCst), 3);
        // Each road's calls counted: cdpd's methods only, the machine's not.
        assert_eq!((e.by_port.calls, e.by_door.calls), (6, 0));
        assert!(e.roads().starts_with("cdpd's methods: 6 by its port in "), "{}", e.roads());
    }

    #[test]
    fn without_cdpds_port_everything_goes_to_cdp_host() {
        let (host, _) = end("host");
        let mut e = Ends::new(Cdp::connect_ws(&host, "/").unwrap(), None);
        assert_eq!(from(&mut e, "DOM.getDocument"), "host");
        assert_eq!(from(&mut e, "Input.dispatchKeyEvent"), "host");
        assert_eq!((e.by_port.calls, e.by_door.calls), (0, 1), "cdpd's method counted on COM3's road");
    }

    #[test]
    fn a_port_that_never_answers_is_an_error() {
        let (host, _) = end("host");
        // A port nothing listens on.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
        let mut e = Ends::new(Cdp::connect_ws(&host, "/").unwrap(), Some(closed));
        e.wait = Duration::from_millis(300);
        let err = e.call("DOM.getDocument", Value::Obj(Vec::new())).unwrap_err();
        assert!(err.contains("never answered"), "{err}");
    }
}

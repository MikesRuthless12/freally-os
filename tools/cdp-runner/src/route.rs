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
}

impl Ends {
    pub fn new(host: Cdp, cdpd: Option<String>) -> Ends {
        Ends { host, cdpd: cdpd.map(|a| (a, None)), wait: CDPD_WAIT }
    }

    /// Calls `method` at the end that serves it; `Err` carries CDP's error,
    /// or says cdpd's port never answered.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let wait = self.wait;
        let Some((addr, conn)) = self.cdpd.as_mut().filter(|_| cdpd_serves(method)) else {
            return self.host.call(method, params);
        };
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
    }

    #[test]
    fn without_cdpds_port_everything_goes_to_cdp_host() {
        let (host, _) = end("host");
        let mut e = Ends::new(Cdp::connect_ws(&host, "/").unwrap(), None);
        assert_eq!(from(&mut e, "DOM.getDocument"), "host");
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

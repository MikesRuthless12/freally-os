// SPDX-License-Identifier: Apache-2.0
//! brain-bridge (TASK-082, FR-036): the MCP server Claude Code launches. A
//! pipe between its stdio and the dev VM's COM2 socket (`127.0.0.1:4552`),
//! adding nothing but reconnection:
//!
//! - it tries every 500 ms for 30 s, says on stderr when it's connected, and
//!   gives up with *"No Freally OS dev VM is listening on 127.0.0.1:4552."*;
//! - each connection starts with a blank line, so a line the Brain had
//!   half-read before a drop is thrown away, not glued to the next;
//! - only whole lines reach stdout: a reply cut off by a drop is dropped,
//!   never handed to the client half-written;
//! - what the client sends while the VM is away (a reboot) waits and goes
//!   once it's back; when the client closes stdin, the replies still on
//!   their way are delivered and it ends.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// The dev VM's COM2 (PRD § 4).
pub const DEFAULT_PORT: u16 = 4552;

/// How long and how often it tries to reach the VM.
#[derive(Clone, Copy, Debug)]
pub struct Patience {
    pub every: Duration,
    pub for_: Duration,
}

impl Default for Patience {
    fn default() -> Patience {
        Patience { every: Duration::from_millis(500), for_: Duration::from_secs(30) }
    }
}

enum Event {
    /// A line from the client (its newline included).
    In(Vec<u8>),
    /// The client closed stdin.
    InputClosed,
    /// Connection `n` ended.
    Dropped(u64),
}

fn connect(addr: SocketAddr, p: Patience) -> Result<TcpStream, String> {
    let start = Instant::now();
    loop {
        if let Ok(s) = TcpStream::connect_timeout(&addr, p.every) {
            return Ok(s);
        }
        if start.elapsed() >= p.for_ {
            return Err(format!("No Freally OS dev VM is listening on {addr}."));
        }
        thread::sleep(p.every);
    }
}

/// Copies whole lines from the socket to `out` until the socket ends; a
/// last, unfinished line is dropped.
fn replies(sock: TcpStream, out: &Mutex<Box<dyn Write + Send>>) {
    let mut r = BufReader::new(sock);
    let mut line = Vec::new();
    loop {
        line.clear();
        match r.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if line.last() != Some(&b'\n') => return,
            Ok(_) => {
                let Ok(mut o) = out.lock() else { return };
                if o.write_all(&line).and_then(|_| o.flush()).is_err() {
                    return;
                }
            }
        }
    }
}

/// Runs the bridge: `input` (the client's stdin) to the VM, the VM's lines
/// to `output`, what it's doing to `log`. Returns when the client closes its
/// input, or with the error when the VM can't be reached.
pub fn run(
    addr: SocketAddr,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
    log: &mut dyn Write,
    patience: Patience,
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<Event>();
    let to_vm = tx.clone();
    thread::spawn(move || {
        let mut r = BufReader::new(input);
        loop {
            let mut line = Vec::new();
            match r.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => {
                    let _ = to_vm.send(Event::InputClosed);
                    return;
                }
                Ok(_) => {
                    if line.last() != Some(&b'\n') {
                        line.push(b'\n');
                    }
                    if to_vm.send(Event::In(line)).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let output: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(output)));
    let mut waiting: Option<Vec<u8>> = None;
    for n in 0u64.. {
        let mut sock = connect(addr, patience)?;
        let _ = writeln!(log, "brain-bridge: connected to the Brain on {addr}");
        let reader = sock.try_clone().map_err(|e| e.to_string())?;
        let (out, done) = (output.clone(), tx.clone());
        let replies_thread = thread::spawn(move || {
            replies(reader, &out);
            let _ = done.send(Event::Dropped(n));
        });
        let mut ok = sock.write_all(b"\n").is_ok();
        if ok && let Some(line) = waiting.take() {
            ok = sock.write_all(&line).is_ok();
            if !ok {
                waiting = Some(line);
            }
        }
        while ok {
            match next(&rx) {
                Event::In(line) => {
                    if sock.write_all(&line).is_err() {
                        waiting = Some(line);
                        ok = false;
                    }
                }
                Event::InputClosed => {
                    // The client is done: let the replies still coming arrive.
                    let _ = sock.shutdown(Shutdown::Write);
                    let _ = replies_thread.join();
                    return Ok(());
                }
                Event::Dropped(m) if m == n => ok = false,
                Event::Dropped(_) => {}
            }
        }
        let _ = sock.shutdown(Shutdown::Both);
        let _ = replies_thread.join();
        let _ = writeln!(log, "brain-bridge: the VM went away; trying again");
    }
    Ok(())
}

fn next(rx: &Receiver<Event>) -> Event {
    rx.recv().unwrap_or(Event::InputClosed)
}

/// `--port N` (a VM started with a port offset), else [`DEFAULT_PORT`]. The
/// host is always this machine: the Brain is never reached from another.
pub fn port_arg(args: &[String]) -> Result<u16, String> {
    match args.iter().position(|a| a == "--port") {
        None => Ok(DEFAULT_PORT),
        Some(i) => {
            args.get(i + 1).and_then(|p| p.parse().ok()).ok_or_else(|| String::from("--port needs a port number"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::TcpListener;

    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn quick() -> Patience {
        Patience { every: Duration::from_millis(20), for_: Duration::from_millis(300) }
    }

    /// A stand-in Brain: each connection reads lines (skipping blank ones),
    /// answers `n` of them in upper case (or all, until the client is done),
    /// and hangs up.
    fn brain(per_connection: Vec<usize>) -> (SocketAddr, thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = thread::spawn(move || {
            let mut seen = Vec::new();
            for n in per_connection {
                let (s, _) = l.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut w = s;
                let mut answered = 0;
                while answered < n {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap() == 0 {
                        break;
                    }
                    if line.trim().is_empty() {
                        seen.push(String::from("<blank>"));
                        continue;
                    }
                    seen.push(line.trim().to_string());
                    w.write_all(line.to_uppercase().as_bytes()).unwrap();
                    answered += 1;
                }
                // A reply cut off by the drop: never reaches the client.
                let _ = w.write_all(b"{\"half");
            }
            seen
        });
        (addr, h)
    }

    /// The client's stdin, written to over time.
    struct Pipe {
        rx: mpsc::Receiver<Vec<u8>>,
        buf: Vec<u8>,
    }

    impl Read for Pipe {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.buf.is_empty() {
                match self.rx.recv() {
                    Ok(b) => self.buf = b,
                    Err(_) => return Ok(0),
                }
            }
            let n = out.len().min(self.buf.len());
            out[..n].copy_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            Ok(n)
        }
    }

    fn wait_for(out: &Shared, text: &str) {
        let start = Instant::now();
        while !String::from_utf8_lossy(&out.0.lock().unwrap()).contains(text) {
            assert!(start.elapsed() < Duration::from_secs(5), "never saw {text}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn lines_cross_and_survive_a_reconnect() {
        // The second connection stays up until the client is done, as a VM does.
        let (addr, h) = brain(vec![1, usize::MAX]);
        let out = Shared::default();
        let (tx, rx) = mpsc::channel();
        let o = out.clone();
        let bridge = thread::spawn(move || {
            let mut log = Vec::new();
            let r = run(addr, Pipe { rx, buf: Vec::new() }, o, &mut log, quick());
            (r, String::from_utf8(log).unwrap())
        });
        tx.send(b"{\"id\":1}\n".to_vec()).unwrap();
        wait_for(&out, "{\"ID\":1}\n");
        // The VM hangs up after its answer (a reboot); the next request waits for it.
        thread::sleep(Duration::from_millis(100));
        tx.send(b"{\"id\":2}\n".to_vec()).unwrap();
        wait_for(&out, "{\"ID\":2}\n");
        drop(tx);
        let (r, log) = bridge.join().unwrap();
        r.unwrap();
        // Whole lines only: the half-sent replies never reached the client.
        assert_eq!(String::from_utf8(out.0.lock().unwrap().clone()).unwrap(), "{\"ID\":1}\n{\"ID\":2}\n");
        // Each connection began with a blank line.
        assert_eq!(h.join().unwrap(), ["<blank>", "{\"id\":1}", "<blank>", "{\"id\":2}"]);
        assert_eq!(log.matches("connected to the Brain").count(), 2, "{log}");
        assert!(log.contains("the VM went away"), "{log}");
    }

    #[test]
    fn no_vm_is_said_plainly() {
        // A port nothing listens on: bind one, then let it go.
        let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let mut log = Vec::new();
        let err = run(addr, std::io::empty(), Shared::default(), &mut log, quick()).unwrap_err();
        assert_eq!(err, format!("No Freally OS dev VM is listening on {addr}."));
    }
}

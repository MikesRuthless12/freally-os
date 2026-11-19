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
//!
//! **The side door** (`127.0.0.1:4562`, TASK-085): while Claude Code holds
//! the line, QEMU's socket takes no one else, so `cargo xtask brain-swap
//! <service> --send` reaches the Brain through the bridge: its lines go to
//! the VM like the client's, and every answer whose id starts `side:` comes
//! back to it, never to Claude Code. A build is megabytes of base64, far
//! more than a model can carry in a tool call, so a swap goes this way.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// The dev VM's COM2 (PRD § 4).
pub const DEFAULT_PORT: u16 = 4552;
/// The side door is COM2's port plus this.
pub const SIDE_OFFSET: u16 = 10;
/// How every answer to the side door begins (the Brain writes `jsonrpc`,
/// then `id`).
pub const SIDE_ANSWER: &[u8] = b"{\"jsonrpc\":\"2.0\",\"id\":\"side:";

/// The side door's one client, if one is connected.
type Side = Arc<Mutex<Option<TcpStream>>>;

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
    /// A line from the client or the side door (its newline included).
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

/// Copies whole lines from the socket to `out` (the side door's answers to
/// it) until the socket ends; a last, unfinished line is dropped.
fn replies(sock: TcpStream, out: &Mutex<Box<dyn Write + Send>>, side: &Side) {
    let mut r = BufReader::new(sock);
    let mut line = Vec::new();
    loop {
        line.clear();
        match r.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if line.last() != Some(&b'\n') => return,
            Ok(_) if line.starts_with(SIDE_ANSWER) => {
                if let Ok(mut s) = side.lock()
                    && let Some(c) = s.as_mut()
                    && c.write_all(&line).is_err()
                {
                    *s = None;
                }
            }
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
/// to `output`, what it's doing to `log`, and the side door's lines if it
/// has one. Returns when the client closes its input, or with the error when
/// the VM can't be reached.
pub fn run(
    addr: SocketAddr,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
    log: &mut dyn Write,
    patience: Patience,
    side_door: Option<TcpListener>,
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<Event>();
    let side: Side = Arc::new(Mutex::new(None));
    if let Some(l) = side_door {
        let (side, to_vm) = (side.clone(), tx.clone());
        thread::spawn(move || {
            for c in l.incoming().map_while(Result::ok) {
                let Ok(reader) = c.try_clone() else { continue };
                if let Ok(mut s) = side.lock() {
                    *s = Some(c);
                }
                for mut line in BufReader::new(reader).split(b'\n').map_while(Result::ok) {
                    line.push(b'\n');
                    if to_vm.send(Event::In(line)).is_err() {
                        return;
                    }
                }
            }
        });
    }
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
        let (out, done, side) = (output.clone(), tx.clone(), side.clone());
        let replies_thread = thread::spawn(move || {
            replies(reader, &out, &side);
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

/// The bridge as Claude Code runs it: stdio to COM2 on `--port` (default
/// 4552), the side door on that plus [`SIDE_OFFSET`] when it's free.
pub fn serve(args: &[String]) -> Result<(), String> {
    use std::net::Ipv4Addr;
    let port = port_arg(args)?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let side = TcpListener::bind((Ipv4Addr::LOCALHOST, port + SIDE_OFFSET));
    if let Err(e) = &side {
        eprintln!(
            "brain-bridge: no side door on port {} ({e}); brain-swap --send won't reach this bridge",
            port + SIDE_OFFSET
        );
    }
    run(addr, std::io::stdin(), std::io::stdout(), &mut std::io::stderr(), Patience::default(), side.ok())
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
            let r = run(addr, Pipe { rx, buf: Vec::new() }, o, &mut log, quick(), None);
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
    fn the_side_door_gets_its_own_answers_and_no_others() {
        // A stand-in Brain that answers each line with itself.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut w = s.try_clone().unwrap();
            for line in BufReader::new(s).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    w.write_all(format!("{line}\n").as_bytes()).unwrap();
                }
            }
        });
        let door = TcpListener::bind("127.0.0.1:0").unwrap();
        let door_addr = door.local_addr().unwrap();
        let out = Shared::default();
        let (tx, rx) = mpsc::channel();
        let o = out.clone();
        let bridge =
            thread::spawn(move || run(addr, Pipe { rx, buf: Vec::new() }, o, &mut Vec::new(), quick(), Some(door)));
        let side = TcpStream::connect(door_addr).unwrap();
        let mut side_w = side.try_clone().unwrap();
        side_w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"side:1\",\"method\":\"x\"}\n").unwrap();
        tx.send(b"{\"jsonrpc\":\"2.0\",\"id\":2}\n".to_vec()).unwrap();
        let mut answer = String::new();
        BufReader::new(side).read_line(&mut answer).unwrap();
        assert_eq!(answer, "{\"jsonrpc\":\"2.0\",\"id\":\"side:1\",\"method\":\"x\"}\n");
        wait_for(&out, "\"id\":2}\n");
        drop(tx);
        drop(side_w);
        let _ = bridge.join();
        assert_eq!(String::from_utf8(out.0.lock().unwrap().clone()).unwrap(), "{\"jsonrpc\":\"2.0\",\"id\":2}\n");
    }

    #[test]
    fn no_vm_is_said_plainly() {
        // A port nothing listens on: bind one, then let it go.
        let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let mut log = Vec::new();
        let err = run(addr, std::io::empty(), Shared::default(), &mut log, quick(), None).unwrap_err();
        assert_eq!(err, format!("No Freally OS dev VM is listening on {addr}."));
    }
}

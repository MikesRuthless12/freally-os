// SPDX-License-Identifier: Apache-2.0
//! The network (Phase 8): each driver proved on QEMU's user network, read
//! back through the test door.

use crate::Ctx;
use crate::terminal::{feed, open, screen_text};
use std::time::{Duration, Instant};

/// Types `line` into the shell once, then waits up to 15 s for the screen to
/// hold every one of `want` (a ping takes a few seconds).
fn shell_until(ctx: &Ctx, line: &str, want: &[&str]) -> Result<String, String> {
    feed(ctx, "type", b"clear\r")?;
    feed(ctx, "type", format!("{line}\r").as_bytes())?;
    let start = Instant::now();
    loop {
        let s = screen_text(ctx)?;
        if want.iter().all(|w| s.contains(w)) {
            return Ok(s);
        }
        if start.elapsed() > Duration::from_secs(15) {
            return Err(format!("`{line}` printed:\n{s}\n(wanted {want:?})"));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// `@net.dhcp`: netstack asks DHCP as it starts and is given QEMU's
/// 10.0.2.15/24 for a day; the shell's `ip` says so. The VM's network is
/// restricted (it never reaches beyond QEMU), and a restricted network's
/// DHCP gives no router and no DNS server: `fnet`'s host tests read those
/// (TASK-174, FR-060).
pub fn dhcp(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("netstack: 10.0.2.15/24 from 10.0.2.2 by DHCP, router none, DNS none, lease 86400 s")?;
    open(ctx)?;
    shell_until(
        ctx,
        "ip",
        &["eth0  drv-virtio-net  52:54:00:12:34:56  link up", "  inet 10.0.2.15/24  lease 86400 s"],
    )?;
    Ok(())
}

/// `@net.ping`: `ping 10.0.2.2` is answered three times (the gateway found
/// by ARP first); an address no one has gets no reply, said, in time; a
/// mistyped address is refused with the usage (TASK-174).
pub fn ping(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("netstack: 10.0.2.15/24 from 10.0.2.2 by DHCP")?;
    open(ctx)?;
    shell_until(
        ctx,
        "ping 10.0.2.2",
        &["reply from 10.0.2.2: seq=1 time=", "reply from 10.0.2.2: seq=3 time=", "3 sent, 3 answered."],
    )?;
    shell_until(ctx, "ping 10.0.2.99 1", &["no reply from 10.0.2.99: seq=1", "1 sent, 0 answered."])?;
    shell_until(ctx, "ping 10.0.2", &["isn't an IPv4 or IPv6 address", "usage: ping <address> [count]"])?;
    Ok(())
}

/// The door's `netdev stats` counts: (rx frames, tx frames).
fn counts(ctx: &Ctx) -> Result<(u64, u64), String> {
    let lines = ctx.door("netdev stats")?;
    let l = lines.iter().find_map(|l| l.strip_prefix("stats ")).ok_or(format!("netdev stats said {lines:?}"))?;
    let field = |name: &str| {
        l.split(' ')
            .find_map(|w| w.strip_prefix(name))
            .and_then(|v| v.split('/').next())
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or(format!("no {name} in {l:?}"))
    };
    Ok((field("rx=")?, field("tx=")?))
}

/// The driver `name` up with QEMU's MAC and the link; the door's `netdev
/// info` agreeing; the gateway's ARP answered, the counts moving.
fn driver_up(ctx: &mut Ctx, name: &str) -> Result<(), String> {
    ctx.expect_log(&format!("{name}: up, MAC 52:54:00:12:34:56, link up"))?;
    let info = ctx.door("netdev info")?;
    let want = format!("info {name} mac=52:54:00:12:34:56 mtu=1500 link=up");
    if info.first() != Some(&want) {
        return Err(format!("netdev info said {info:?}, not {want:?}"));
    }
    let (rx0, tx0) = counts(ctx)?;
    let reply = ctx.door("netdev arp 10.0.2.2")?;
    if !reply.iter().any(|l| l.starts_with("reply 10.0.2.2 is-at ")) {
        return Err(format!("no answer from the gateway: {reply:?}"));
    }
    let (rx1, tx1) = counts(ctx)?;
    if tx1 <= tx0 || rx1 <= rx0 {
        return Err(format!("the counts didn't move: rx {rx0} → {rx1}, tx {tx0} → {tx1}"));
    }
    Ok(())
}

/// `@net.e1000`: drv-e1000 on QEMU's `e1000` (an 82540EM, as VirtualBox's
/// default card): QEMU's MAC, the link up, the gateway's ARP answered, and
/// netstack's address by DHCP over it (TASK-173).
pub fn e1000(ctx: &mut Ctx) -> Result<(), String> {
    driver_up(ctx, "drv-e1000")?;
    ctx.expect_log("netstack: up on drv-e1000 (eth0), asking DHCP")?;
    ctx.expect_log("netstack: 10.0.2.15/24 from 10.0.2.2 by DHCP")
}

/// `@net.e1000e`: the same on QEMU's `e1000e` (an 82574L) (TASK-173).
pub fn e1000e(ctx: &mut Ctx) -> Result<(), String> {
    e1000(ctx)
}

/// `@net.virtio`: drv-virtio-net comes up on a virtio-net-pci card with
/// QEMU's MAC (52:54:00:12:34:56), the link up; an ARP request for the
/// gateway (10.0.2.2) goes out and its answer comes back (the frames read
/// back from the driver's counts) (TASK-172, FR-060).
pub fn virtio(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("drv-virtio-net: up, MAC 52:54:00:12:34:56, link up")?;
    settled(ctx)?;
    let info = ctx.door("netdev info")?;
    let want = "info drv-virtio-net mac=52:54:00:12:34:56 mtu=1500 link=up";
    if info.first().map(String::as_str) != Some(want) {
        return Err(format!("netdev info said {info:?}, not {want:?}"));
    }
    let (rx0, tx0) = counts(ctx)?;
    let reply = ctx.door("netdev arp 10.0.2.2")?;
    let gateway = reply
        .iter()
        .find_map(|l| l.strip_prefix("reply 10.0.2.2 is-at "))
        .ok_or(format!("no answer from the gateway: {reply:?}"))?
        .to_string();
    if gateway.len() != 17 || gateway == "00:00:00:00:00:00" {
        return Err(format!("the gateway's MAC {gateway:?}"));
    }
    let (rx1, tx1) = counts(ctx)?;
    if tx1 <= tx0 || rx1 <= rx0 {
        return Err(format!("the counts didn't move: rx {rx0} → {rx1}, tx {tx0} → {tx1}"));
    }
    // An address no one answers: said, not waited for forever.
    let none = ctx.door("netdev arp 10.0.2.99")?;
    if !none.iter().any(|l| l.contains("no ARP reply in 2 s")) {
        return Err(format!("an unanswered ARP said {none:?}"));
    }
    Ok(())
}

/// `@net.ipv6`: netstack's link-local address (a random interface id, checked
/// by DAD), a global one by SLAAC from QEMU's router advertisement (fec0::/64),
/// both in the shell's `ip`, and `ping fec0::2` answered over IPv6, its
/// neighbour found by NDP (TASK-175).
pub fn ipv6(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("netstack: fe80::")?;
    ctx.expect_log("/64 by SLAAC, router fe80::")?;
    open(ctx)?;
    shell_until(ctx, "ip", &["  inet6 fe80::", "  inet6 fec0::"])?;
    shell_until(ctx, "ping fec0::2", &["reply from fec0::2: seq=1 time=", "3 sent, 3 answered."])?;
    Ok(())
}

/// `@net.tcp`: a connection through netstack to QEMU's own TCP, which hands
/// it to `cat` on the host (the guestfwd at 10.0.2.100:7, the echo port):
/// 1 MiB sent and echoed back byte for byte, then both sides closed, the
/// peer's FIN read as the end. A closed port's RST is "refused"; a SYN no
/// one answers (no one has 10.0.2.99) is given up at its deadline, said (TASK-177).
pub fn tcp(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("netstack: 10.0.2.15/24 from 10.0.2.2 by DHCP")?;
    settled(ctx)?;
    let echoed = ctx.door("tcp echo 10.0.2.100 7 1048576")?;
    let line = echoed.iter().find(|l| l.starts_with("echoed 1048576 bytes in "));
    let line = line.ok_or_else(|| format!("tcp echo said {echoed:?}"))?;
    let ms: f64 = line.trim_start_matches("echoed 1048576 bytes in ").trim_end_matches(" ms").parse().unwrap_or(0.0);
    println!("    1 MiB echoed through TCP in {ms:.0} ms ({:.1} MB/s each way)", 1.048_576 / (ms.max(1.0) / 1000.0));
    // QEMU's own TCP answers a SYN to a closed port with a RST.
    let shut = ctx.door("tcp connect 10.0.2.2 9 2000")?;
    if !shut.iter().any(|l| l.starts_with("error: ") && l.contains("refused")) {
        return Err(format!("a closed port said {shut:?}"));
    }
    let start = Instant::now();
    let silent = ctx.door("tcp connect 10.0.2.99 9 2000")?;
    if !silent.iter().any(|l| l.starts_with("error: ") && l.contains("no answer")) {
        return Err(format!("a connection no one answers said {silent:?}"));
    }
    if start.elapsed() > Duration::from_secs(6) {
        return Err(format!("no answer took {:?} to say, for a 2 s deadline", start.elapsed()));
    }
    Ok(())
}

/// OpenSSL's tool, for the TLS spec's certificates and server.
fn openssl(dir: &std::path::Path, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("openssl")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| format!("@net.tls needs OpenSSL's command line tool: {e}"))?;
    if !out.status.success() {
        return Err(format!("openssl {args:?}: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// A root and a test.freally.local leaf (P-256) made by OpenSSL in `dir`: the
/// root's DER in hex. Valid from the minute before (TASK-180A: the guest must
/// know the true time, its zone set, to take them) for ten years.
pub(crate) fn pki(dir: &std::path::Path) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stamp = |secs: u64| {
        let (y, mo, d, h, mi, s) = crate::settings::civil(secs);
        format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}Z")
    };
    let now = crate::settings::host_now();
    let (from, to) = (stamp(now - 60), stamp(now + 10 * 365 * 86_400));
    for key in ["root.key", "leaf.key"] {
        openssl(dir, &["genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", key])?;
    }
    openssl(
        dir,
        &[
            "req",
            "-x509",
            "-new",
            "-key",
            "root.key",
            "-subj",
            "/CN=Freally Test Root",
            "-not_before",
            &from,
            "-not_after",
            &to,
            "-sha256",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign",
            "-out",
            "root.pem",
        ],
    )?;
    openssl(dir, &["req", "-new", "-key", "leaf.key", "-subj", "/CN=test.freally.local", "-out", "leaf.csr"])?;
    std::fs::write(dir.join("leaf.ext"), "subjectAltName=DNS:test.freally.local\nextendedKeyUsage=serverAuth\n")
        .map_err(|e| e.to_string())?;
    openssl(
        dir,
        &[
            "x509",
            "-req",
            "-in",
            "leaf.csr",
            "-CA",
            "root.pem",
            "-CAkey",
            "root.key",
            "-set_serial",
            "7",
            "-not_before",
            &from,
            "-not_after",
            &to,
            "-sha256",
            "-extfile",
            "leaf.ext",
            "-out",
            "leaf.pem",
        ],
    )?;
    openssl(dir, &["x509", "-in", "root.pem", "-outform", "DER", "-out", "root.der"])?;
    let der = std::fs::read(dir.join("root.der")).map_err(|e| e.to_string())?;
    Ok(der.iter().map(|b| format!("{b:02x}")).collect())
}

/// `@net.tls`: ftls in the guest, over netstack's TCP, to OpenSSL's s_server
/// on the host (QEMU hands 10.0.2.100:443 to `nc`, which reaches the
/// server): each cipher suite connects with the chain checked against the
/// root the door is given, and `GET /` is answered whole (s_server's `-www`
/// page); a wrong name and an unknown root are refused (TASK-180).
pub fn tls(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("netstack: 10.0.2.15/24 from 10.0.2.2 by DHCP")?;
    let port = ctx.tls_port.ok_or("no --tls-port for this VM")?;
    // The zone first (QEMU's RTC keeps the host's local time): without it a
    // certificate made the minute before isn't valid yet (TASK-180A).
    let off = crate::settings::host_zone()?;
    let said = ctx.door(&format!("zone {off} local"))?;
    if said != ["ok"] {
        return Err(format!("zone {off} local: {said:?}"));
    }
    let base = std::env::temp_dir().join(format!("freally-net-tls-{port}"));
    let _ = std::fs::remove_dir_all(&base);
    let (root, other) = (pki(&base.join("a"))?, pki(&base.join("b"))?);
    for (suite, name) in [
        ("TLS_CHACHA20_POLY1305_SHA256", "ChaCha20Poly1305Sha256"),
        ("TLS_AES_128_GCM_SHA256", "Aes128GcmSha256"),
        ("TLS_AES_256_GCM_SHA384", "Aes256GcmSha384"),
    ] {
        let server = Server::start(&base.join("a"), port, suite)?;
        let said = ctx.door(&format!("tls get 10.0.2.100 443 test.freally.local {root} /"))?;
        drop(server);
        let want = [format!("tls {name} test.freally.local"), "http 200 ok".to_string()];
        if said.len() < 3 || said[..2] != want || !said[2].starts_with("body ") {
            return Err(format!("{suite}: the door said {said:?}"));
        }
        ctx.note(&format!("{suite}: {}", said[2]));
    }
    for (host, r, want) in
        [("other.freally.local", &root, "certificate Name"), ("test.freally.local", &other, "certificate UnknownRoot")]
    {
        let server = Server::start(&base.join("a"), port, "TLS_AES_128_GCM_SHA256")?;
        let said = ctx.door(&format!("tls get 10.0.2.100 443 {host} {r} /"))?;
        drop(server);
        if !said.iter().any(|l| l.starts_with("error: ") && l.contains(want)) {
            return Err(format!("{host}, {want}: the door said {said:?}"));
        }
    }
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}

/// One `openssl s_server -www` on 127.0.0.1:`port`, stopped when dropped.
struct Server(std::process::Child);

impl Server {
    fn start(dir: &std::path::Path, port: u16, suite: &str) -> Result<Server, String> {
        let child = std::process::Command::new("openssl")
            .current_dir(dir)
            .args(["s_server", "-accept", &format!("127.0.0.1:{port}"), "-tls1_3", "-cert", "leaf.pem"])
            .args(["-key", "leaf.key", "-www", "-quiet", "-ciphersuites", suite])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("openssl s_server: {e}"))?;
        let s = Server(child);
        let start = Instant::now();
        // Listening once a probe connects (s_server takes it and moves on).
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            if start.elapsed() > Duration::from_secs(10) {
                return Err("s_server never listened".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(s)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `@net.dns`: on the test network (a TAP device; e2e/os/net/testnet.py gives
/// the address by DHCP, and the DNS server), the shell's `nslookup`: an A;
/// a CNAME followed to it; an AAAA; both kinds; a name with 40 addresses
/// (too big for UDP: TC, then TCP, all 40); a chain the server leaves in its
/// answers, asked again for its end; a name that doesn't exist (TASK-176).
pub fn dns(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("netstack: 10.77.")?;
    let dhcp = ctx.log();
    let Some(line) = dhcp.lines().find(|l| l.contains("by DHCP")) else { return Err("no DHCP lease said".into()) };
    let prefix =
        line.split("netstack: ").nth(1).and_then(|r| r.split(".15/").next()).ok_or("an odd lease line")?.to_string();
    if !line.contains(&format!("DNS {prefix}.1")) {
        return Err(format!("the lease gave another DNS server: {line}"));
    }
    open(ctx)?;
    let one = |name: &str, want: &[&str]| -> Result<(), String> {
        let wanted: Vec<String> = want.iter().map(|w| format!("{name} has address {w}")).collect();
        let refs: Vec<&str> = wanted.iter().map(String::as_str).collect();
        shell_until(ctx, &format!("nslookup {name}"), &refs).map(|_| ())
    };
    let me = format!("{prefix}.1");
    one("test.freally.local", &[&me])?;
    one("www.freally.local", &[&me])?;
    one("six.freally.local", &["fec0::1"])?;
    one("both.freally.local", &[&format!("{prefix}.7"), "fec0::7"])?;
    // 40 lines scroll past the screen: the last two-thirds of them stay on it,
    // and the 40th only comes whole over TCP.
    one("big.freally.local", &[&format!("{prefix}.120"), &format!("{prefix}.139")])?;
    one("alias.freally.local", &[&format!("{prefix}.9")])?;
    shell_until(ctx, "nslookup none.freally.local", &["nslookup: none.freally.local: no such name"])?;
    Ok(())
}

/// The test network's HTTPS server (`e2e/os/net/https.py`) on the TAP's host
/// address, stopped when dropped.
pub(crate) struct Https(std::process::Child);

impl Https {
    pub(crate) fn start(dir: &std::path::Path, address: &str) -> Result<Https, String> {
        let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../e2e/os/net/https.py");
        let mut child = std::process::Command::new("python3")
            .arg(&script)
            .arg(dir)
            .arg(address)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| format!("https.py: {e}"))?;
        // Serving once it says so.
        let mut out = std::io::BufReader::new(child.stdout.take().ok_or("https.py: no stdout")?);
        let mut line = String::new();
        std::io::BufRead::read_line(&mut out, &mut line).map_err(|e| format!("https.py: {e}"))?;
        if !line.starts_with("https: serving") {
            let _ = child.kill();
            return Err(format!("https.py said {line:?}"));
        }
        Ok(Https(child))
    }
}

impl Drop for Https {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Waits (30 s) for this boot's desktop to be up (S4 on conductor's boot
/// line). A door command runs inside conductor, and these specs' hold it for
/// seconds (an address no one answers, a listener waiting); a service
/// starting meanwhile can't reach conductor's directory in its 2 s, and ends.
/// The desktop asks the directory until its Rack is up (for stage, which
/// registers after its first frame, then its art), so S3 is too soon: the
/// desktop ended with `@net.tcp` first in its VM, then `@net.virtio`.
fn settled(ctx: &Ctx) -> Result<(), String> {
    let start = Instant::now();
    while !desktop_up(&ctx.log()) {
        if start.elapsed() > Duration::from_secs(30) {
            return Err("this boot's desktop never came up (no S4)".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Whether the log's last boot has its desktop up: S4 on conductor's boot line.
fn desktop_up(log: &str) -> bool {
    let boot = log.rfind("freally-boot ").unwrap_or(0);
    log[boot..].lines().any(|l| l.contains("conductor: boot: ") && l.contains(" S4="))
}

/// The test network's first three octets (`10.77.<n>`), from netstack's
/// lease: the host is `.1` on it, the guest `.15`.
fn testnet_prefix(ctx: &mut Ctx) -> Result<String, String> {
    ctx.expect_log("netstack: 10.77.")?;
    settled(ctx)?;
    let log = ctx.log();
    let line = log.lines().find(|l| l.contains("by DHCP")).ok_or("no DHCP lease said")?;
    Ok(line.split("netstack: ").nth(1).and_then(|r| r.split(".15/").next()).ok_or("an odd lease line")?.to_string())
}

/// `@net.limits` (TASK-185): one client holds at most 16 TCP connections at
/// once: on the test network, against a listener on the host that keeps
/// every connection open, the 17th is refused ("too many connections"),
/// and once they're closed a new one opens.
pub fn limits(ctx: &mut Ctx) -> Result<(), String> {
    let prefix = testnet_prefix(ctx)?;
    let host = format!("{prefix}.1");
    let script = format!(
        "import socket,time\ns=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\ns.bind(('{host}', 7777)); s.listen(64)\nprint('listening', flush=True)\nheld=[]\nwhile True:\n    c,_=s.accept(); held.append(c)\n"
    );
    let mut child = std::process::Command::new("python3")
        .args(["-c", &script])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("python3: {e}"))?;
    let mut out = std::io::BufReader::new(child.stdout.take().ok_or("no stdout")?);
    let mut first = String::new();
    let _ = std::io::BufRead::read_line(&mut out, &mut first);
    let said = ctx.door(&format!("tcp hold {host} 7777 20"));
    let again = ctx.door(&format!("tcp hold {host} 7777 1"));
    let _ = child.kill();
    let _ = child.wait();
    let said = said?;
    if said.first().map(String::as_str) != Some("opened 16 of 20")
        || !said.get(1).is_some_and(|w| w.contains("too many connections"))
    {
        return Err(format!("20 asked for: {said:?}"));
    }
    let again = again?;
    if again != ["opened 1 of 1"] {
        return Err(format!("once closed, a new connection wasn't let open: {again:?}"));
    }
    Ok(())
}

/// `@net.waiting` (TASK-185): one client has at most 8 pings and 8 lookups
/// waiting at once. Ten pings sent together to an address nobody has on the
/// test network: 2 s later 8 are waiting (their 3 s) and 2 were refused ("too
/// many pings waiting"); ten lookups of names its DNS never answers: the
/// same (netstack's 10 s); once those are over, one of each is taken again.
pub fn waiting(ctx: &mut Ctx) -> Result<(), String> {
    let prefix = testnet_prefix(ctx)?;
    let nobody = format!("{prefix}.250");
    for (what, ask) in [("pings", format!("ping {nobody}")), ("lookups", String::from("resolve"))] {
        let said = ctx.door(&format!("net waiting {ask} 10"))?;
        if said.first().map(String::as_str) != Some("waiting 8, refused 2, answered 0")
            || said.get(1).map(String::as_str) != Some(&*format!("too many {what} waiting"))
        {
            return Err(format!("10 {what} at once: {said:?}"));
        }
        // Refused while the eight wait; taken once they're over.
        let start = std::time::Instant::now();
        loop {
            let again = ctx.door(&format!("net waiting {ask} 1"))?;
            if again.first().map(String::as_str) == Some("waiting 1, refused 0, answered 0") {
                break;
            }
            if start.elapsed() > std::time::Duration::from_secs(15) {
                return Err(format!("15 s on, a new one of the {what} still isn't taken: {again:?}"));
            }
        }
    }
    Ok(())
}

/// A Python client on the host, its script run with the guest's address as
/// `G`; what it prints, once it ends.
fn host_client(script: &str, guest: &str) -> Result<std::process::Child, String> {
    std::process::Command::new("python3")
        .args(["-c", &format!("G = {guest:?}\n{script}")])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("python3: {e}"))
}

fn printed(child: std::process::Child) -> Result<String, String> {
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let (o, e) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        return Err(format!("the host's client: {}{}", o.trim(), e.trim()));
    }
    Ok(o.trim().to_string())
}

/// `@net.listen` (TASK-184): netstack listens, in e2e images, for the test
/// clients alone. On the test network the host connects to the guest: the
/// door's listener takes two connections in turn and echoes each (the
/// handshake from the host, 100 KB both ways, both ends closed); with a
/// backlog of 2 and nothing accepted, two of five connections are taken and
/// three get no answer, then the two are accepted; a program that isn't a
/// test client may not listen.
pub fn listen(ctx: &mut Ctx) -> Result<(), String> {
    let prefix = testnet_prefix(ctx)?;
    let (guest, host) = (format!("{prefix}.15"), format!("{prefix}.1"));
    // Connects as soon as the guest listens (refused until then).
    let connect = "import socket,time\ndef connect(port, wait):\n    end = time.time() + 10\n    while True:\n        s = socket.socket(); s.settimeout(wait)\n        try:\n            s.connect((G, port)); return s\n        except ConnectionRefusedError:\n            s.close()\n            if time.time() > end: raise\n            time.sleep(0.1)\n";
    let echo = format!(
        "{connect}data = bytes((i * 7 + i // 251) % 256 for i in range(100000))\nfor _ in range(2):\n    s = connect(7000, 5); s.settimeout(10); s.sendall(data); s.shutdown(socket.SHUT_WR)\n    got = b''\n    while True:\n        b = s.recv(65536)\n        if not b: break\n        got += b\n    assert got == data, (len(got), len(data))\n    s.close()\nprint('echoed twice')\n"
    );
    let child = host_client(&echo, &guest)?;
    let said = ctx.door("tcp serve 7000 2");
    let host_said = printed(child);
    let said = said?;
    if said != [format!("served 2 from {host}, 200000 bytes echoed")] || host_said.as_deref() != Ok("echoed twice") {
        return Err(format!("two connections echoed: the door said {said:?}, the host {host_said:?}"));
    }
    // Five at once against a backlog of 2: two handshakes, three SYNs dropped.
    let flood = format!(
        "{connect}import threading\nres, held = [], []\ndef one():\n    try:\n        held.append(connect(7001, 2)); res.append('ok')\n    except socket.timeout:\n        res.append('timeout')\nts = [threading.Thread(target=one) for _ in range(5)]\n[t.start() for t in ts]; [t.join() for t in ts]\nprint(res.count('ok'), 'ok,', res.count('timeout'), 'timed out', flush=True)\ntime.sleep(4)\n"
    );
    let child = host_client(&flood, &guest)?;
    let said = ctx.door("tcp backlog 7001 2 5");
    let host_said = printed(child);
    let said = said?;
    if said != ["accepted 2"] || host_said.as_deref() != Ok("2 ok, 3 timed out") {
        return Err(format!("a backlog of 2: the door said {said:?}, the host {host_said:?}"));
    }
    let said = ctx.door("tcp listen-as terminal 7002")?;
    if !said.first().is_some_and(|l| l.contains("only the test clients listen")) {
        return Err(format!("the Terminal asked to listen: {said:?}"));
    }
    let said = ctx.door("tcp listen-as cdpd 7002")?;
    if said != ["listening"] {
        return Err(format!("cdpd asked to listen: {said:?}"));
    }
    Ok(())
}

/// `@cdp.network` (TASK-184): cdpd's port on the network. A WebSocket
/// client on the host (the test network reaches the guest) opens
/// `ws://<guest>:9222/` (the accept key checked), asks `Target.getTargets`
/// and gets the desktop first by its id, asks what isn't served and gets an
/// error by its id; takes a screenshot (a PNG the size of QEMU's screen),
/// moves the mouse (the pointer follows) and presses Tab (the focus moves;
/// or, the Terminal having it, types a word the shell answers), all
/// through stage; its ping is answered with a pong and its close
/// echoed; a request that isn't WebSocket is answered 400.
pub fn cdp_network(ctx: &mut Ctx) -> Result<(), String> {
    let prefix = testnet_prefix(ctx)?;
    ctx.expect_log("cdpd: CDP over WebSocket on port 9222")?;
    let client = host_client(CDP_CLIENT, &format!("{prefix}.15"))?;
    let said = printed(client)?;
    let words: Vec<&str> = said.strip_prefix("CDP over WebSocket ").unwrap_or("").split(' ').collect();
    let size: Vec<u32> = words.iter().take(2).filter_map(|n| n.parse().ok()).collect();
    // cdpd's screenshot (stage's picture) the size of cdp-host's (QEMU's).
    let shot = ctx.screenshot()?;
    if size != [shot.w, shot.h] {
        return Err(format!("the host's WebSocket client said {said:?}; QEMU's screen is {}x{}", shot.w, shot.h));
    }
    // Keys typed into the Terminal: the shell answered `ws6` (no such command).
    if words.get(2) == Some(&"terminal") {
        let start = std::time::Instant::now();
        while !crate::terminal::screen_text(ctx)?.lines().any(|l| l.trim().starts_with("ws6: ")) {
            if start.elapsed() > std::time::Duration::from_secs(10) {
                return Err(format!(
                    "`ws6` typed over WebSocket never answered: the terminal {:?}; stage {:?}",
                    ctx.door("terminal screen"),
                    ctx.door("stage list")
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    Ok(())
}

/// `@cdp.network`'s client: WebSocket written out (RFC 6455), so the host
/// needs nothing but Python.
const CDP_CLIENT: &str = r#"import base64, hashlib, json, os, socket, struct
def opened():
    s = socket.create_connection((G, 9222), 10)
    key = base64.b64encode(os.urandom(16)).decode()
    s.sendall(('GET / HTTP/1.1\r\nHost: freally\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\n\r\n' % key).encode())
    head = b''
    while b'\r\n\r\n' not in head:
        b = s.recv(1)
        assert b, head
        head += b
    want = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
    assert head.startswith(b'HTTP/1.1 101 ') and ('Sec-WebSocket-Accept: ' + want).encode() in head, head
    return s
def exactly(s, n):
    b = b''
    while len(b) < n:
        c = s.recv(n - len(b))
        assert c, 'closed early'
        b += c
    return b
def send(s, op, data):
    mask = os.urandom(4)
    n = len(data)
    head = bytes([0x80 | op]) + (bytes([0x80 | n]) if n < 126 else bytes([0x80 | 126]) + struct.pack('!H', n))
    s.sendall(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))
def recv(s):
    b0, b1 = exactly(s, 2)
    assert b1 & 0x80 == 0, 'a server frame masked'
    n = b1 & 0x7F
    if n == 126: n = struct.unpack('!H', exactly(s, 2))[0]
    elif n == 127: n = struct.unpack('!Q', exactly(s, 8))[0]
    return b0 & 0x0F, exactly(s, n)
s = opened()
send(s, 1, json.dumps({'id': 1, 'method': 'Target.getTargets'}).encode())
op, p = recv(s)
m = json.loads(p)
assert op == 1 and m['id'] == 1 and m['result']['targetInfos'][0]['targetId'] == 'desktop', m
send(s, 1, b'{"id":2,"method":"No.such"}')
m = json.loads(recv(s)[1])
assert m['id'] == 2 and m['error']['code'] == -32000, m
def call(i, method, params=None):
    send(s, 1, json.dumps({'id': i, 'method': method, 'params': params or {}}).encode())
    r = json.loads(recv(s)[1])
    assert r['id'] == i and 'result' in r, r
    return r['result']
png = base64.b64decode(call(3, 'Page.captureScreenshot', {'format': 'png'})['data'])
assert png[:8] == b'\x89PNG\r\n\x1a\n' and png[12:16] == b'IHDR', png[:16]
W, H = struct.unpack('!II', png[16:24])
at = call(4, 'Freally.nodeAt')['at']
dx = 40 if at['x'] < W // 2 else -40
dy = 30 if at['y'] < H // 2 else -30
call(5, 'Input.dispatchMouseEvent', {'type': 'mouseMoved', 'deltaX': dx, 'deltaY': dy})
to = call(6, 'Freally.nodeAt')['at']
assert (to['x'] - at['x']) * dx > 0 and (to['y'] - at['y']) * dy > 0, (at, to, dx, dy)
before = call(7, 'Freally.focusedNode')
def press(code):
    for kind in ('keyDown', 'keyUp'):
        call(8, 'Input.dispatchKeyEvent', {'type': kind, 'code': code})
if (before.get('node') or {}).get('testid') == 'terminal-screen':
    # The Terminal takes Tab itself: `ws6` typed instead (the spec reads the shell's answer).
    for code in ['KeyW', 'KeyS', 'Digit6', 'Enter']:
        press(code)
    keys = 'terminal'
else:
    press('Tab')
    # The key reaches the app through stage; its tree says so once it has run.
    import time
    end = time.time() + 3
    after = before
    while after == before and time.time() < end:
        time.sleep(0.1)
        after = call(9, 'Freally.focusedNode')
    assert after != before, ('Tab moved no focus', before)
    keys = 'focus'
send(s, 9, b'are you there')
assert recv(s) == (10, b'are you there')
send(s, 8, struct.pack('!H', 1000))
assert recv(s) == (8, struct.pack('!H', 1000))
s.close()
t = socket.create_connection((G, 9222), 10)
t.sendall(b'GET / HTTP/1.1\r\nHost: freally\r\n\r\n')
r = t.recv(100)
assert r.startswith(b'HTTP/1.1 400 '), r
print('CDP over WebSocket', W, H, keys)
"#;

/// `@net.dns6` (TASK-176): DNS over IPv6 alone. This test network's lease
/// names no DNS server; its router advertisements give an address (SLAAC)
/// and, by RDNSS, the IPv6 server fd78:<n>::1: `ip` shows it, and the
/// shell's nslookup gets its answers from it, over IPv6.
pub fn dns6(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("netstack: 10.78.")?;
    let log = ctx.log();
    let lease = log.lines().find(|l| l.contains("by DHCP")).ok_or("no DHCP lease said")?;
    if !lease.contains("DNS none") {
        return Err(format!("the lease named a DNS server: {lease}"));
    }
    let prefix =
        lease.split("netstack: ").nth(1).and_then(|r| r.split(".15/").next()).ok_or("an odd lease line")?.to_string();
    let n = prefix.rsplit('.').next().unwrap_or_default().to_string();
    ctx.expect_log(&format!("netstack: fd78:{n}:"))?;
    open(ctx)?;
    shell_until(ctx, "ip", &[&format!("dns fd78:{n}::1")])?;
    shell_until(ctx, "nslookup test.freally.local", &[&format!("test.freally.local has address {prefix}.1")])?;
    shell_until(ctx, "nslookup six.freally.local", &["six.freally.local has address fec0::1"])?;
    Ok(())
}

/// `@net.grants` (TASK-183): netstack serves the programs its list names
/// and no one else: a client called `terminal` is served; one called
/// `intruder` is refused, and the refusal is said in the log.
pub fn grants(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("netstack: 10.0.2.15/24 from 10.0.2.2 by DHCP")?;
    let said = ctx.door("netdev as terminal")?;
    if said != ["served"] {
        return Err(format!("terminal: {said:?}"));
    }
    let n = ctx.log_count("netstack: intruder may not use the network");
    let said = ctx.door("netdev as intruder")?;
    if said != ["refused"] {
        return Err(format!("intruder: {said:?}"));
    }
    crate::session::waits_for_log(ctx, "netstack: intruder may not use the network (not on the Network page)", n)
        .map(|_| ())
}

/// `@net.https` (TASK-181): the shell's `fetch` from the Terminal, on the
/// test network: test.freally.local resolved by its DNS, TLS 1.3 to the
/// host's HTTPS server, the chain checked against the root `--root` names (a
/// PEM file written to the session's home, the certificates made the minute
/// before); a 300,000-byte page read whole, its SHA-256 the host's, by
/// Content-Length and chunked; a redirect followed and said; a redirect loop
/// given up after five; no root given: refused (the image trusts none yet);
/// another root: refused.
pub fn https(ctx: &mut Ctx) -> Result<(), String> {
    let prefix = testnet_prefix(ctx)?;
    let off = crate::settings::host_zone()?;
    if ctx.door(&format!("zone {off} local"))? != ["ok"] {
        return Err("the zone wasn't set".into());
    }
    crate::terminal::session(ctx, "tester")?;
    let base = std::env::temp_dir().join(format!("freally-net-https-{prefix}"));
    let _ = std::fs::remove_dir_all(&base);
    let (_, _) = (pki(&base.join("a"))?, pki(&base.join("b"))?);
    // The page: 300,000 bytes no compressor would shrink.
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let page: Vec<u8> = (0..300_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    std::fs::write(base.join("a/page.bin"), &page).map_err(|e| e.to_string())?;
    let sum: String = fcrypto::sha2::sha256(&page).iter().map(|b| format!("{b:02x}")).collect();
    let _server = Https::start(&base.join("a"), &format!("{prefix}.1"))?;
    open(ctx)?;
    // The roots, as PEM files in the home (their lines joined by spaces: one
    // typed line; fetch reads PEM whatever its whitespace).
    for (name, dir) in [("root.pem", "a"), ("other.pem", "b")] {
        let pem = std::fs::read_to_string(base.join(dir).join("root.pem")).map_err(|e| e.to_string())?;
        let one: Vec<&str> = pem.split_whitespace().collect();
        shell_until(ctx, &format!("write /home/tester/{name} {}", one.join(" ")), &[])?;
        shell_until(ctx, &format!("cat /home/tester/{name}"), &["-----END CERTIFICATE-----"])?;
    }
    let root = "--root /home/tester/root.pem";
    let page_line = "https://test.freally.local/: 200 OK, 300000 bytes";
    let hash = format!("sha256 {sum}");
    shell_until(ctx, &format!("fetch {root} https://test.freally.local/"), &[page_line, &hash])?;
    shell_until(
        ctx,
        &format!("fetch {root} https://test.freally.local/chunked"),
        &["https://test.freally.local/chunked: 200 OK, 300000 bytes", &hash],
    )?;
    shell_until(
        ctx,
        &format!("fetch {root} https://test.freally.local/moved"),
        &["redirected to https://test.freally.local/", page_line, &hash],
    )?;
    shell_until(ctx, &format!("fetch {root} https://test.freally.local/loop"), &["more than 5 redirects"])?;
    shell_until(ctx, "fetch https://test.freally.local/", &["this image trusts no root yet"])?;
    shell_until(ctx, "fetch --root /home/tester/other.pem https://test.freally.local/", &["certificate UnknownRoot"])?;
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}

#[cfg(test)]
mod settled_tests {
    use super::desktop_up;

    const BOOT: &str = "freally-boot 0.8.0 (e2e)\n";
    const S3: &str = "[conductor] conductor: boot: B0=0.0 S2=457.9 S3=644.5 ms after B0; firmware times: no FPDT\n";
    const S4: &str =
        "[conductor] conductor: boot: B0=0.0 S2=457.9 S3=644.5 S4=2657.7 ms after B0; firmware times: no FPDT\n";

    #[test]
    fn the_desktop_is_up_only_once_this_boot_has_s4() {
        // Stage's first frame: the desktop is still asking conductor for stage and its art.
        assert!(!desktop_up(&format!("{BOOT}{S3}")));
        assert!(desktop_up(&format!("{BOOT}{S3}{S4}")));
        // The last boot's S4 says nothing about this one.
        assert!(!desktop_up(&format!("{BOOT}{S3}{S4}{BOOT}{S3}")));
    }
}

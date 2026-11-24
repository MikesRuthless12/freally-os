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

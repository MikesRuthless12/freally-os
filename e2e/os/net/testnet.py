# SPDX-License-Identifier: Apache-2.0
# A test network for one VM (TASK-176): on the TAP device xtask made for it
# (its address <prefix>.1/24), a DHCP server (the VM gets <prefix>.15, the DNS
# server <prefix>.1, no router: nothing leaves this link) and a DNS server
# over UDP and TCP with a fixed zone. Linux answers ARP. A host tool, run as
# root inside WSL by xtask; it ends with its parent.
#   testnet.py <tap> <prefix>          e.g. testnet.py frt2 10.77.2
#   testnet.py <tap> <prefix> --v6     IPv6's DNS alone (TASK-176): the lease
#     names no DNS server; router advertisements give the prefix fd78:<n>::/64
#     and, by RDNSS (RFC 8106), the server fd78:<n>::1, which xtask put on the
#     tap; the same zone answers there over UDP.
import ctypes
import signal
import socket
import socketserver
import struct
import sys
import threading

tap, prefix = sys.argv[1], sys.argv[2]
v6 = "--v6" in sys.argv[3:]
me = f"{prefix}.1"
guest = f"{prefix}.15"
# End with xtask (PR_SET_PDEATHSIG = 1).
ctypes.CDLL("libc.so.6", use_errno=True).prctl(1, signal.SIGTERM)

# The zone: name -> list of (type, data). big has 40 addresses: more than a
# UDP answer holds (512 bytes), so UDP says TC and TCP gives them all.
A, CNAME, AAAA = 1, 5, 28
ZONE = {
    "test.freally.local": [(A, socket.inet_aton(me))],
    "www.freally.local": [(CNAME, "test.freally.local")],
    "six.freally.local": [(AAAA, socket.inet_pton(socket.AF_INET6, "fec0::1"))],
    "both.freally.local": [(A, socket.inet_aton(f"{prefix}.7")), (AAAA, socket.inet_pton(socket.AF_INET6, "fec0::7"))],
    "big.freally.local": [(A, socket.inet_aton(f"{prefix}.{100 + i}")) for i in range(40)],
    # A CNAME whose target this server answers only when asked for it.
    "alias.freally.local": [(CNAME, "elsewhere.freally.local")],
    "elsewhere.freally.local": [(A, socket.inet_aton(f"{prefix}.9"))],
}


def wire_name(n):
    return b"".join(bytes([len(p)]) + p.encode() for p in n.split(".")) + b"\0"


def read_name(m, at):
    labels = []
    while m[at]:
        labels.append(m[at + 1:at + 1 + m[at]].decode().lower())
        at += 1 + m[at]
    return ".".join(labels), at + 1


def answer(query, udp):
    qid, flags = struct.unpack("!HH", query[:4])
    name, at = read_name(query, 12)
    qtype = struct.unpack("!H", query[at:at + 2])[0]
    question = query[12:at + 4]
    records = ZONE.get(name)
    if records is None:
        return struct.pack("!HHHHHH", qid, 0x8183, 1, 0, 0, 0) + question
    out = []
    follow = name not in ("alias.freally.local",)
    at_name = name
    for _ in range(8):
        recs = ZONE.get(at_name, [])
        cname = [r for r in recs if r[0] == CNAME]
        hits = [r for r in recs if r[0] == qtype]
        for t, d in hits:
            out.append(wire_name(at_name) + struct.pack("!HHIH", t, 1, 300, len(d)) + d)
        if hits or not cname:
            break
        target = cname[0][1]
        data = wire_name(target)
        out.append(wire_name(at_name) + struct.pack("!HHIH", CNAME, 1, 60, len(data)) + data)
        if not follow:
            break
        at_name = target
    body = b"".join(out)
    flags = 0x8180
    if udp and 12 + len(question) + len(body) > 512:
        return struct.pack("!HHHHHH", qid, flags | 0x0200, 1, 0, 0, 0) + question
    return struct.pack("!HHHHHH", qid, flags, 1, len(out), 0, 0) + question + body


def silent(query):
    """A question under silent.freally.local: never answered, so a lookup
    of it waits its whole time (TASK-185: what a client has waiting)."""
    try:
        return read_name(query, 12)[0].endswith(".silent.freally.local")
    except Exception:
        return False


class Udp(socketserver.BaseRequestHandler):
    def handle(self):
        data, sock = self.request
        if silent(data):
            return
        try:
            sock.sendto(answer(data, True), self.client_address)
        except Exception as e:  # a malformed query: no answer
            print("testnet: udp:", e, file=sys.stderr)


class Tcp(socketserver.StreamRequestHandler):
    def handle(self):
        try:
            head = self.rfile.read(2)
            if not head:  # xtask's "is it up?": connected, sent nothing
                return
            n = struct.unpack("!H", head)[0]
            r = answer(self.rfile.read(n), False)
            self.wfile.write(struct.pack("!H", len(r)) + r)
        except Exception as e:
            print("testnet: tcp:", e, file=sys.stderr)


class Reuse:
    allow_reuse_address = True


class UdpServer(Reuse, socketserver.ThreadingUDPServer):
    pass


class TcpServer(Reuse, socketserver.ThreadingTCPServer):
    pass


def dhcp():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, tap.encode())
    s.bind(("0.0.0.0", 67))
    while True:
        m, _ = s.recvfrom(1500)
        if len(m) < 240 or m[0] != 1 or m[236:240] != b"\x63\x82\x53\x63":
            continue
        kind, i = None, 240
        while i < len(m) and m[i] != 255:
            if m[i] == 0:
                i += 1
                continue
            if m[i] == 53:
                kind = m[i + 2]
            i += 2 + m[i + 1]
        reply = {1: 2, 3: 5}.get(kind)  # DISCOVER -> OFFER, REQUEST -> ACK
        if reply is None:
            continue
        r = bytearray(240)
        r[0], r[1], r[2] = 2, 1, 6
        r[4:8] = m[4:8]  # xid
        r[10:12] = m[10:12]  # flags
        r[16:20] = socket.inet_aton(guest)
        r[20:24] = socket.inet_aton(me)
        r[28:44] = m[28:44]  # chaddr
        r[236:240] = b"\x63\x82\x53\x63"
        opts = bytes([53, 1, reply]) + bytes([54, 4]) + socket.inet_aton(me)
        opts += bytes([51, 4]) + struct.pack("!I", 86400) + bytes([1, 4]) + socket.inet_aton("255.255.255.0")
        if not v6:
            opts += bytes([6, 4]) + socket.inet_aton(me)
        opts += b"\xff"
        s.sendto(bytes(r) + opts, ("255.255.255.255", 68))


six = f"fd78:{prefix.rsplit('.', 1)[1]}::1"


def router_ads():
    """Router advertisements on the tap: the prefix fd78:<n>::/64 (on the
    link, for SLAAC) and RDNSS naming `six`; at once for each solicitation,
    and every 3 s. Not a default router (lifetime 0): the server is on the link."""
    s = socket.socket(socket.AF_INET6, socket.SOCK_RAW, socket.IPPROTO_ICMPV6)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, tap.encode())
    s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_MULTICAST_HOPS, 255)
    s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_UNICAST_HOPS, 255)
    s.settimeout(3)
    mac = bytes.fromhex(open(f"/sys/class/net/{tap}/address").read().strip().replace(":", ""))
    ad = bytes([134, 0, 0, 0, 64, 0, 0, 0]) + bytes(8)
    ad += bytes([1, 1]) + mac
    ad += bytes([3, 4, 64, 0xC0]) + struct.pack("!II", 86400, 14400) + bytes(4)
    ad += socket.inet_pton(socket.AF_INET6, f"fd78:{prefix.rsplit('.', 1)[1]}::")
    ad += bytes([25, 3, 0, 0]) + struct.pack("!I", 3600) + socket.inet_pton(socket.AF_INET6, six)
    index = socket.if_nametoindex(tap)
    while True:
        s.sendto(ad, ("ff02::1", 0, 0, index))
        try:
            m, _ = s.recvfrom(1500)
            while m[:1] != b"\x85":  # 133: a router solicitation
                m, _ = s.recvfrom(1500)
        except socket.timeout:
            pass


class Udp6Server(Reuse, socketserver.ThreadingUDPServer):
    address_family = socket.AF_INET6


threading.Thread(target=dhcp, daemon=True).start()
threading.Thread(target=UdpServer((me, 53), Udp).serve_forever, daemon=True).start()
if v6:
    threading.Thread(target=router_ads, daemon=True).start()
    threading.Thread(target=Udp6Server((six, 53), Udp).serve_forever, daemon=True).start()
print(f"testnet: {tap} {me}: DHCP and DNS up" + (f", IPv6's DNS at {six}" if v6 else ""), flush=True)
TcpServer((me, 53), Tcp).serve_forever()

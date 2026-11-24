# SPDX-License-Identifier: Apache-2.0
# Pulls the values ftls's tests check out of RFC 8448 § 3 (the simple 1-RTT
# handshake), so none is typed by hand. A host tool only.
#   curl -sS -o /tmp/rfc8448.txt https://www.rfc-editor.org/rfc/rfc8448.txt
#   python3 lib/ftls/tests/gen_rfc8448.py /tmp/rfc8448.txt
# Each line of rfc8448.txt: name hex
import os
import re
import sys

here = os.path.dirname(os.path.abspath(__file__))
text = open(sys.argv[1]).read().split("\n")
start = text.index("3.  Simple 1-RTT Handshake")
end = text.index("4.  Resumed 0-RTT Handshake")
steps = []  # (who, step text, [(field, hex)])
field = None
for line in text[start:end]:
    if line.startswith(("Thomson", "RFC 8448")) or "\f" in line:
        continue
    m = re.match(r"^   \{(client|server)\}\s+(.*)$", line)
    if m:
        steps.append((m.group(1), m.group(2), []))
        field = None
        continue
    m = re.match(r"^      (.+?) \((\d+) octets\):\s*(.*)$", line)
    if m and steps:
        hexes = "" if m.group(3) == "(empty)" else m.group(3).replace(" ", "")
        field = [m.group(1), hexes, int(m.group(2))]
        steps[-1][2].append(field)
        continue
    m = re.match(r"^         ([0-9a-f ]+)$", line)
    if m and field is not None:
        field[1] += m.group(1).replace(" ", "")
        continue
    if line.strip() and steps and not steps[-1][2]:
        steps[-1] = (steps[-1][0], steps[-1][1] + " " + line.strip(), steps[-1][2])
    elif line.strip():
        field = None

for s in steps:
    for f in s[2]:
        assert len(f[1]) == 2 * f[2], (s[1], f[0], len(f[1]), f[2])


def pick(who, step, name, nth=0):
    found = [f[1] for s in steps if s[0] == who and step in s[1] for f in s[2] if f[0] == name]
    return found[nth]


rows = [
    ("client_x25519", pick("client", "create an ephemeral x25519", "private key")),
    ("server_x25519_public", pick("server", "create an ephemeral x25519", "public key")),
    ("client_hello", pick("client", "construct a ClientHello", "ClientHello")),
    ("server_hello", pick("server", "construct a ServerHello", "ServerHello")),
    ("client_hello_record", pick("client", "send handshake record", "complete record")),
    ("server_hello_record", pick("server", "send handshake record", "complete record")),
    ("handshake_secret", pick("server", 'extract secret "handshake"', "secret")),
    ("client_hs", pick("server", '"tls13 c hs traffic"', "expanded")),
    ("server_hs", pick("server", '"tls13 s hs traffic"', "expanded")),
    ("master_secret", pick("server", 'extract secret "master"', "secret")),
    ("server_hs_key", pick("server", "derive write traffic keys for handshake data", "key expanded")),
    ("server_hs_iv", pick("server", "derive write traffic keys for handshake data", "iv expanded")),
    ("server_flight", pick("server", "send handshake record", "payload", 1)),
    ("server_flight_record", pick("server", "send handshake record", "complete record", 1)),
    ("server_finished", pick("server", 'calculate finished "tls13 finished"', "finished")),
    ("client_ap", pick("server", '"tls13 c ap traffic"', "expanded")),
    ("server_ap", pick("server", '"tls13 s ap traffic"', "expanded")),
    ("client_finished", pick("client", 'calculate finished "tls13 finished"', "finished")),
    ("client_finished_record", pick("client", "send handshake record", "complete record", 1)),
    ("server_ticket", pick("server", "send handshake record", "payload", 2)),
    ("server_ticket_record", pick("server", "send handshake record", "complete record", 2)),
    ("app_data", pick("client", "send application_data record", "payload")),
    ("client_app_record", pick("client", "send application_data record", "complete record")),
    ("server_app_record", pick("server", "send application_data record", "complete record")),
    ("client_alert_record", pick("client", "send alert record", "complete record")),
    ("server_alert_record", pick("server", "send alert record", "complete record")),
]
with open(os.path.join(here, "rfc8448.txt"), "w") as f:
    for name, h in rows:
        f.write(f"{name} {h}\n")
print(len(rows), "values from", len(steps), "steps")

# SPDX-License-Identifier: Apache-2.0
# P-256 ECDH cases for fcrypto's tests (TASK-180), made by an independent
# implementation: Python's cryptography (OpenSSL underneath). A host tool only.
#   python3 lib/fcrypto/tests/gen_p256.py
# Each line of p256_cases.txt: secret our-public peer-public shared (hex).
# The secrets include the edges: 1, n - 1, and 32 bytes at or past n
# (reduced mod n, as fcrypto does).
import os
import secrets

from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

here = os.path.dirname(os.path.abspath(__file__))
n = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551
rows = []
edges = [1, 2, n - 1, n + 1, 2**256 - 1]
for i in range(24):
    raw = edges[i] if i < len(edges) else secrets.randbelow(2**256)
    k = raw % n
    if k == 0:
        continue
    ours = ec.derive_private_key(k, ec.SECP256R1())
    peer = ec.generate_private_key(ec.SECP256R1())
    shared = ours.exchange(ec.ECDH(), peer.public_key())
    pub = lambda key: key.public_key().public_bytes(Encoding.X962, PublicFormat.UncompressedPoint).hex()
    rows.append((raw.to_bytes(32, "big").hex(), pub(ours), pub(peer), shared.hex()))
with open(os.path.join(here, "p256_cases.txt"), "w") as f:
    for r in rows:
        f.write(" ".join(r) + "\n")
print(len(rows), "cases")

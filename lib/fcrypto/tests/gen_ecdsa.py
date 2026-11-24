# SPDX-License-Identifier: Apache-2.0
# ECDSA cases for fcrypto's tests (TASK-178), made by an independent
# implementation: Python's cryptography (OpenSSL underneath). A host tool only.
#   python3 lib/fcrypto/tests/gen_ecdsa.py
# It first checks ecdsa.rs's curve constants in plain Python (G on the curve,
# n.G the point at infinity) and stops if they're wrong.
# Each line of ecdsa_cases.txt: curve x y hash digest r s (hex; curve 256/384).
import os
import random
import re

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, utils

here = os.path.dirname(os.path.abspath(__file__))
src = open(os.path.join(here, "..", "src", "ecdsa.rs")).read()


def consts(name):
    block = re.search(name + r": Params = Params \{(.*?)\};", src, re.S).group(1)
    return {k: int(v, 16) for k, v in re.findall(r'(\w+): "([0-9a-f]+)"', block)}


def check(c):
    p, n, b, gx, gy = c["p"], c["n"], c["b"], c["gx"], c["gy"]
    assert (gy * gy - (gx**3 - 3 * gx + b)) % p == 0, "G is not on the curve"

    def add(P, Q):
        if P is None:
            return Q
        if Q is None:
            return P
        (x1, y1), (x2, y2) = P, Q
        if x1 == x2 and (y1 + y2) % p == 0:
            return None
        if P == Q:
            m = (3 * x1 * x1 - 3) * pow(2 * y1, -1, p)
        else:
            m = (y2 - y1) * pow(x2 - x1, -1, p)
        x3 = (m * m - x1 - x2) % p
        return (x3, (m * (x1 - x3) - y1) % p)

    R, Q, k = None, (gx, gy), n
    while k:
        if k & 1:
            R = add(R, Q)
        Q, k = add(Q, Q), k >> 1
    assert R is None, "n.G is not the point at infinity"


for name in ["P256", "P384"]:
    check(consts(name))

rng = random.Random(1795511000)
curves = {256: (ec.SECP256R1(), 32), 384: (ec.SECP384R1(), 48)}
algs = {256: hashes.SHA256(), 384: hashes.SHA384(), 512: hashes.SHA512()}
with open(os.path.join(here, "ecdsa_cases.txt"), "w") as f:
    for cid, (curve, size) in curves.items():
        for k in range(4):
            key = ec.generate_private_key(curve)
            pub = key.public_key().public_numbers()
            for h, alg in algs.items():
                d = hashes.Hash(alg)
                d.update(bytes(rng.getrandbits(8) for _ in range(rng.randint(0, 100))))
                digest = d.finalize()
                r, s = utils.decode_dss_signature(key.sign(digest, ec.ECDSA(utils.Prehashed(alg))))
                f.write(
                    f"{cid} {pub.x.to_bytes(size, 'big').hex()} {pub.y.to_bytes(size, 'big').hex()} "
                    f"{h} {digest.hex()} {r.to_bytes(size, 'big').hex()} {s.to_bytes(size, 'big').hex()}\n"
                )
print("constants checked; cases written")

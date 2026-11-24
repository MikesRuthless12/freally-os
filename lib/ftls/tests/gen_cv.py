# SPDX-License-Identifier: Apache-2.0
# CertificateVerify signatures for ftls's tests, made by an independent
# implementation (Python's cryptography): over RFC 8446 § 4.4.3's content
# with a fixed transcript hash. A host tool only.
#   python3 lib/ftls/tests/gen_cv.py
# Each line of cv.txt: scheme kind key-hex signature-hex
# (kind ec: the key is x||y; kind rsa: the key is n, e = 65537).
import os

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, padding, rsa

here = os.path.dirname(os.path.abspath(__file__))
TRANSCRIPT = bytes(range(32))
content = b"\x20" * 64 + b"TLS 1.3, server CertificateVerify\x00" + TRANSCRIPT
rows = []
for scheme, curve, h, size in [(0x0403, ec.SECP256R1(), hashes.SHA256(), 32), (0x0503, ec.SECP384R1(), hashes.SHA384(), 48)]:
    k = ec.generate_private_key(curve)
    n = k.public_key().public_numbers()
    key = n.x.to_bytes(size, "big") + n.y.to_bytes(size, "big")
    rows.append((scheme, "ec", key, k.sign(content, ec.ECDSA(h))))
rk = rsa.generate_private_key(public_exponent=65537, key_size=2048)
nb = rk.public_key().public_numbers().n.to_bytes(256, "big")
for scheme, h in [(0x0804, hashes.SHA256()), (0x0805, hashes.SHA384()), (0x0806, hashes.SHA512())]:
    pss = padding.PSS(mgf=padding.MGF1(h), salt_length=h.digest_size)
    rows.append((scheme, "rsa", nb, rk.sign(content, pss, h)))
with open(os.path.join(here, "cv.txt"), "w") as f:
    for scheme, kind, key, sig in rows:
        f.write(f"{scheme:04x} {kind} {key.hex()} {sig.hex()}\n")
print(len(rows), "signatures")

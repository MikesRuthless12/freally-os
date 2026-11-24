# SPDX-License-Identifier: Apache-2.0
# Wycheproof's test vectors (C2SP/wycheproof, Apache-2.0) in the compact
# lines fcrypto's and fx509's tests read (TASK-178). A host tool only.
#   mkdir -p /tmp/wyche && cd /tmp/wyche && for f in <the files below>; do
#     curl -sS -o $f.json https://raw.githubusercontent.com/C2SP/wycheproof/main/testvectors_v1/$f.json; done
#   python3 lib/fcrypto/tests/gen_wycheproof.py /tmp/wyche
# Lines: "key <family> <fields>" sets the key the tests after it use;
# "<family> <tcId> <valid|invalid|acceptable> <fields>" is a test; "-" is empty.
import json
import os
import sys

src = sys.argv[1]
here = os.path.dirname(os.path.abspath(__file__))
x509 = os.path.join(here, "..", "..", "fx509", "tests")


def load(name):
    return json.load(open(os.path.join(src, name + ".json")))


def h(s):
    return s if s else "-"


def fixed(hexstr, size):
    return int(hexstr, 16).to_bytes(size, "big").hex() if hexstr else "00" * size


HEADER = "# From Wycheproof (github.com/C2SP/wycheproof, Apache-2.0), by gen_wycheproof.py.\n"
lines = []
for name, fam in [("aes_gcm_test", "gcm"), ("chacha20_poly1305_test", "chacha")]:
    for g in load(name)["testGroups"]:
        for t in g["tests"]:
            lines.append(f"{fam} {t['tcId']} {t['result']} {g['ivSize']} {g['tagSize']} "
                         f"{h(t['key'])} {h(t['iv'])} {h(t['aad'])} {h(t['msg'])} {h(t['ct'])} {h(t['tag'])}")
for g in load("ecdh_secp256r1_ecpoint_test")["testGroups"]:
    for t in g["tests"]:
        lines.append(f"ecdh256 {t['tcId']} {t['result']} {fixed(t['private'], 32)} {h(t['public'])} {h(t['shared'])}")
for g in load("x25519_test")["testGroups"]:
    for t in g["tests"]:
        lines.append(f"x25519 {t['tcId']} {t['result']} {t['private']} {t['public']} {t['shared']}")
for name, fam in [("rsa_signature_2048_sha256_test", "pkcs1"), ("rsa_pss_2048_sha256_mgf1_32_test", "pss")]:
    for g in load(name)["testGroups"]:
        k = g["publicKey"]
        lines.append(f"key {fam} {k['modulus']} {k['publicExponent']}")
        for t in g["tests"]:
            lines.append(f"{fam} {t['tcId']} {t['result']} {h(t['msg'])} {h(t['sig'])}")
with open(os.path.join(here, "wycheproof.txt"), "w") as f:
    f.write(HEADER + "\n".join(lines) + "\n")

ecdsa = []
for name, curve, size in [("ecdsa_secp256r1_sha256_test", "p256", 32), ("ecdsa_secp384r1_sha384_test", "p384", 48)]:
    for g in load(name)["testGroups"]:
        k = g["publicKey"]
        ecdsa.append(f"key {curve} {fixed(k['wx'], size)} {fixed(k['wy'], size)}")
        for t in g["tests"]:
            ecdsa.append(f"{curve} {t['tcId']} {t['result']} {h(t['msg'])} {h(t['sig'])}")
with open(os.path.join(x509, "wycheproof_ecdsa.txt"), "w") as f:
    f.write(HEADER + "\n".join(ecdsa) + "\n")
print(len(lines), "fcrypto lines,", len(ecdsa), "ECDSA lines")

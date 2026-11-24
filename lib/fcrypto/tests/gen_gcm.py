# SPDX-License-Identifier: Apache-2.0
# AES and AES-GCM cases for fcrypto's tests (TASK-178), made by an
# independent implementation: Python's `cryptography` (OpenSSL underneath).
# A host tool only, never part of Freally OS. Run it to make the files again:
#   python3 lib/fcrypto/tests/gen_gcm.py
# Each line of gcm_cases.txt: key nonce aad plaintext ciphertext tag (hex, "-" for empty).
# Each line of aes_cases.txt: key plaintext ciphertext (hex).
import os
import random

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives.ciphers.aead import AESGCM

here = os.path.dirname(os.path.abspath(__file__))
rng = random.Random(1795509000)


def b(n):
    return bytes(rng.getrandbits(8) for _ in range(n))


def h(x):
    return x.hex() if x else "-"


with open(os.path.join(here, "aes_cases.txt"), "w") as f:
    for i in range(32):
        key, pt = b(16 if i % 2 == 0 else 32), b(16)
        e = Cipher(algorithms.AES(key), modes.ECB()).encryptor()
        f.write(f"{key.hex()} {pt.hex()} {(e.update(pt) + e.finalize()).hex()}\n")

# Lengths around block edges, and a few long ones.
lengths = [0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 65, 255, 256, 1000]
with open(os.path.join(here, "gcm_cases.txt"), "w") as f:
    for i in range(64):
        key = b(16 if i % 2 == 0 else 32)
        nonce = b(12)
        aad = b(rng.choice([0, 1, 13, 16, 20, 64]))
        pt = b(lengths[i % len(lengths)])
        sealed = AESGCM(key).encrypt(nonce, pt, aad)
        ct, tag = sealed[:-16], sealed[-16:]
        f.write(f"{key.hex()} {nonce.hex()} {h(aad)} {h(pt)} {h(ct)} {tag.hex()}\n")

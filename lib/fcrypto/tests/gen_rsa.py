# SPDX-License-Identifier: Apache-2.0
# RSA signature cases for fcrypto's tests (TASK-178), made by an independent
# implementation: Python's cryptography (OpenSSL underneath). A host tool only.
#   python3 lib/fcrypto/tests/gen_rsa.py
# Each line of rsa_cases.txt: n e hash scheme digest signature (hex; hash 256/384/512, scheme pkcs1/pss).
# Moduli of 2049 and 3071 bits reach PSS's leading-zero-byte and leftmost-bits paths.
import os
import random

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import padding, rsa, utils

here = os.path.dirname(os.path.abspath(__file__))
rng = random.Random(1795510000)
algs = {256: hashes.SHA256(), 384: hashes.SHA384(), 512: hashes.SHA512()}

with open(os.path.join(here, 'rsa_cases.txt'), 'w') as f:
    for bits in [2048, 2049, 3071, 4096]:
        key = rsa.generate_private_key(public_exponent=65537, key_size=bits)
        pub = key.public_key().public_numbers()
        n = pub.n.to_bytes((pub.n.bit_length() + 7) // 8, 'big').hex()
        e = pub.e.to_bytes(3, 'big').hex()
        for h, alg in algs.items():
            for scheme in ['pkcs1', 'pss']:
                msg = bytes(rng.getrandbits(8) for _ in range(rng.randint(0, 200)))
                d = hashes.Hash(alg)
                d.update(msg)
                digest = d.finalize()
                pad = padding.PKCS1v15() if scheme == 'pkcs1' else padding.PSS(mgf=padding.MGF1(alg), salt_length=alg.digest_size)
                sig = key.sign(digest, pad, utils.Prehashed(alg))
                f.write(f'{n} {e} {h} {scheme} {digest.hex()} {sig.hex()}\n')

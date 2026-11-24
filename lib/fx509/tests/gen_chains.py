# SPDX-License-Identifier: Apache-2.0
# Certificate chains for fx509's tests (TASK-179), made by an independent
# implementation: Python's cryptography (OpenSSL underneath). A host tool only.
#   python3 lib/fx509/tests/gen_chains.py
# Each line of chains.txt: name expected host now chain roots
# (chain and roots: DER in hex, joined by ","; "-" for none).
import datetime as dt
import os

from cryptography import x509
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, rsa
from cryptography.hazmat.primitives.serialization import Encoding
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

here = os.path.dirname(os.path.abspath(__file__))
utc = dt.timezone.utc
NOW = int(dt.datetime(2026, 11, 24, 9, 30, 15, tzinfo=utc).timestamp())
serial = [1000]


def name(cn):
    return x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, cn)])


def cert(subject, issuer, key, signer, hash_alg, start, end, ca=None, path_len=None, cert_sign=None,
         san=None, eku=None, critical_unknown=False):
    serial[0] += 1
    b = (x509.CertificateBuilder().subject_name(name(subject)).issuer_name(name(issuer))
         .public_key(key.public_key()).serial_number(serial[0])
         .not_valid_before(dt.datetime(*start, tzinfo=utc)).not_valid_after(dt.datetime(*end, tzinfo=utc)))
    if ca is not None:
        b = b.add_extension(x509.BasicConstraints(ca=ca, path_length=path_len), critical=True)
    if cert_sign is not None:
        b = b.add_extension(x509.KeyUsage(digital_signature=not cert_sign, content_commitment=False,
                                          key_encipherment=False, data_encipherment=False, key_agreement=False,
                                          key_cert_sign=cert_sign, crl_sign=cert_sign, encipher_only=False,
                                          decipher_only=False), critical=True)
    if san:
        b = b.add_extension(x509.SubjectAlternativeName([x509.DNSName(n) for n in san]), critical=False)
    if eku:
        b = b.add_extension(x509.ExtendedKeyUsage(eku), critical=False)
    if critical_unknown:
        b = b.add_extension(x509.UnrecognizedExtension(x509.ObjectIdentifier("1.3.6.1.4.1.55555.1"), b"\x05\x00"),
                            critical=True)
    return b.sign(signer, hash_alg).public_bytes(Encoding.DER)


root_key = ec.generate_private_key(ec.SECP384R1())
inter_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
leaf_key = ec.generate_private_key(ec.SECP256R1())
other_key = ec.generate_private_key(ec.SECP256R1())
Y = (2026, 1, 1), (2036, 1, 1)
root = cert("Freally Test Root", "Freally Test Root", root_key, root_key, hashes.SHA384(), *Y, ca=True, cert_sign=True)
inter = cert("Freally Test CA", "Freally Test Root", inter_key, root_key, hashes.SHA384(), *Y, ca=True, path_len=0,
             cert_sign=True)
SAN = ["test.freally.local", "*.wild.freally.local"]
LEAF = (2026, 6, 1), (2027, 6, 1)
leaf = cert("test.freally.local", "Freally Test CA", leaf_key, inter_key, hashes.SHA256(), *LEAF, san=SAN,
            eku=[ExtendedKeyUsageOID.SERVER_AUTH])
forged = cert("test.freally.local", "Freally Test CA", leaf_key, other_key, hashes.SHA256(), *LEAF, san=SAN)
client_only = cert("test.freally.local", "Freally Test CA", leaf_key, inter_key, hashes.SHA256(), *LEAF, san=SAN,
                   eku=[ExtendedKeyUsageOID.CLIENT_AUTH])
odd = cert("test.freally.local", "Freally Test CA", leaf_key, inter_key, hashes.SHA256(), *LEAF, san=SAN,
           critical_unknown=True)
not_ca = cert("Freally Test CA", "Freally Test Root", inter_key, root_key, hashes.SHA384(), *Y, ca=False)
inter2 = cert("Freally Second CA", "Freally Test CA", other_key, inter_key, hashes.SHA256(), *Y, ca=True,
              cert_sign=True)
leaf2 = cert("test.freally.local", "Freally Second CA", leaf_key, other_key, hashes.SHA256(), *LEAF, san=SAN)
other_root_key = ec.generate_private_key(ec.SECP256R1())
other_root = cert("Freally Test Root", "Freally Test Root", other_root_key, other_root_key, hashes.SHA256(), *Y,
                  ca=True, cert_sign=True)


def h(certs):
    return ",".join(c.hex() for c in certs) if certs else "-"


cases = [
    ("good", "ok", "test.freally.local", NOW, [leaf, inter], [root]),
    ("good, the root sent too", "ok", "test.freally.local", NOW, [leaf, inter, root], [root]),
    ("a wildcard's one label", "ok", "a.wild.freally.local", NOW, [leaf, inter], [root]),
    ("upper case", "ok", "TEST.Freally.Local", NOW, [leaf, inter], [root]),
    ("a wildcard's two labels", "Name", "a.b.wild.freally.local", NOW, [leaf, inter], [root]),
    ("a wildcard's parent", "Name", "wild.freally.local", NOW, [leaf, inter], [root]),
    ("another host", "Name", "other.freally.local", NOW, [leaf, inter], [root]),
    ("expired", "Expired", "test.freally.local", int(dt.datetime(2028, 1, 1, tzinfo=utc).timestamp()),
     [leaf, inter], [root]),
    ("not yet valid", "NotYetValid", "test.freally.local", int(dt.datetime(2026, 3, 1, tzinfo=utc).timestamp()),
     [leaf, inter], [root]),
    ("no roots", "UnknownRoot", "test.freally.local", NOW, [leaf, inter], []),
    ("another root of the same name", "UnknownRoot", "test.freally.local", NOW, [leaf, inter], [other_root]),
    ("no intermediate", "UnknownRoot", "test.freally.local", NOW, [leaf], [root]),
    ("a forged leaf", "Signature", "test.freally.local", NOW, [forged, inter], [root]),
    ("an issuer that isn't a CA", "NotCa", "test.freally.local", NOW, [leaf, not_ca], [root]),
    ("past the path length", "NotCa", "test.freally.local", NOW, [leaf2, inter2, inter], [root]),
    ("a client-only leaf", "Usage", "test.freally.local", NOW, [client_only, inter], [root]),
    ("an unknown critical extension", "Critical", "test.freally.local", NOW, [odd, inter], [root]),
]
with open(os.path.join(here, "chains.txt"), "w") as f:
    for n, want, host, now, chain, roots in cases:
        f.write(f"{n.replace(' ', '_')} {want} {host} {now} {h(chain)} {h(roots)}\n")
print(len(cases), "cases")

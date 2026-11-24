# SPDX-License-Identifier: Apache-2.0
# The test network's HTTPS server (TASK-181, @net.https): TLS 1.3 only, the
# leaf and key in DIR (made by the spec with OpenSSL), on ADDRESS:443.
#   /         the page (DIR/page.bin), by Content-Length
#   /chunked  the same page, chunked
#   /moved    301 to https://test.freally.local/
#   /loop     302 to itself
#   anything else: a file under DIR/www (TASK-182: an update index and its
#             packages), or 404
# Every path asked for is written to DIR/requests.log as it comes.
# A host tool, run as root beside testnet.py; it ends with the spec.
# usage: https.py DIR ADDRESS

import http.server
import os
import ssl
import sys

d, address = sys.argv[1], sys.argv[2]
page_path = os.path.join(d, "page.bin")
page = open(page_path, "rb").read() if os.path.exists(page_path) else b""
asked = open(os.path.join(d, "requests.log"), "a", buffering=1)
www = os.path.join(d, "www")


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def moved(self, code, to):
        self.send_response(code)
        self.send_header("Location", to)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_GET(self):
        asked.write(self.path + "\n")
        f = os.path.normpath(os.path.join(www, self.path.lstrip("/")))
        if self.path == "/":
            self.send_response(200)
            self.send_header("Content-Length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)
        elif self.path == "/chunked":
            self.send_response(200)
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            for at in range(0, len(page), 7000):
                piece = page[at:at + 7000]
                self.wfile.write(b"%x\r\n" % len(piece) + piece + b"\r\n")
            self.wfile.write(b"0\r\n\r\n")
        elif self.path == "/moved":
            self.moved(301, "https://test.freally.local/")
        elif self.path == "/loop":
            self.moved(302, "https://test.freally.local/loop")
        elif f.startswith(www + os.sep) and os.path.isfile(f):
            self.send_response(200)
            self.send_header("Content-Length", str(os.path.getsize(f)))
            self.end_headers()
            with open(f, "rb") as src:
                while True:
                    piece = src.read(64 * 1024)
                    if not piece:
                        break
                    self.wfile.write(piece)
        else:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()


ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.minimum_version = ssl.TLSVersion.TLSv1_3
# The client's order, not the server's: ftls offers ChaCha20-Poly1305 first
# while its AES runs in constant-time software (TASK-178: AES-NI later).
ctx.options &= ~ssl.OP_CIPHER_SERVER_PREFERENCE
ctx.load_cert_chain(os.path.join(d, "leaf.pem"), os.path.join(d, "leaf.key"))
server = http.server.ThreadingHTTPServer((address, 443), Handler)
server.socket = ctx.wrap_socket(server.socket, server_side=True)
print("https: serving", flush=True)
server.serve_forever()

# SPDX-License-Identifier: Apache-2.0
# The test network's HTTPS server (TASK-181, @net.https): TLS 1.3 only, the
# leaf and key in DIR (made by the spec with OpenSSL), on ADDRESS:443.
#   /         the page (DIR/page.bin), by Content-Length
#   /chunked  the same page, chunked
#   /moved    301 to https://test.freally.local/
#   /loop     302 to itself
# A host tool, run as root beside testnet.py; it ends with the spec.
# usage: https.py DIR ADDRESS

import http.server
import os
import ssl
import sys

d, address = sys.argv[1], sys.argv[2]
page = open(os.path.join(d, "page.bin"), "rb").read()


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
        else:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()


ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.minimum_version = ssl.TLSVersion.TLSv1_3
ctx.load_cert_chain(os.path.join(d, "leaf.pem"), os.path.join(d, "leaf.key"))
server = http.server.ThreadingHTTPServer((address, 443), Handler)
server.socket = ctx.wrap_socket(server.socket, server_side=True)
print("https: serving", flush=True)
server.serve_forever()

#!/usr/bin/env python3
"""Independent HTTP/1.1 keep-alive server (stdlib only) for the HttpClient pooling tests.

usage: server.py http|https [cert key]
Prints "PORT <n>" once listening, then "CONN <id>" per accepted connection and
"REQ <method> <path> <id>" per request, flushed immediately.
"""
import itertools
import ssl
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ids = itertools.count(1)
lock = threading.Lock()


def log(line):
    with lock:
        print(line, flush=True)


def payload(n):
    return ("0123456789abcdef" * (n // 16 + 1))[:n].encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def setup(self):
        super().setup()
        self.conn_id = next(ids)
        log(f"CONN {self.conn_id}")

    def log_message(self, *args):
        pass

    def reply(self, body, extra=()):
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def handle_any(self):
        log(f"REQ {self.command} {self.path} {self.conn_id}")
        length = int(self.headers.get("Content-Length") or 0)
        data = self.rfile.read(length) if length else b""
        path = self.path
        if path.startswith("/len/"):
            self.reply(payload(int(path.split("/")[2])))
        elif path.startswith("/chunked/"):
            n = int(path.split("/")[2])
            body = payload(n)
            self.send_response(200)
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            for i in range(0, len(body), 1000):
                piece = body[i:i + 1000]
                self.wfile.write(b"%x\r\n" % len(piece) + piece + b"\r\n")
            self.wfile.write(b"0\r\n\r\n")
        elif path == "/close":
            self.reply(b"closing", [("Connection", "close")])
        elif path == "/drop":
            # Valid response, then the server hangs up without announcing it.
            self.reply(b"dropped")
            self.close_connection = True
        elif path == "/echo":
            self.reply(b"echo:" + data)
        elif path == "/empty":
            self.send_response(204)
            self.end_headers()
        elif path == "/slow":
            import time
            time.sleep(0.15)
            self.reply(b"slow")
        else:
            self.reply(b"ok")

    do_GET = do_POST = do_PUT = do_DELETE = handle_any

    def do_HEAD(self):
        log(f"REQ HEAD {self.path} {self.conn_id}")
        self.send_response(200)
        self.send_header("Content-Length", "100")
        self.end_headers()


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 128


def main():
    mode = sys.argv[1]
    srv = Server(("127.0.0.1", 0), Handler)
    if mode == "https":
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(sys.argv[2], sys.argv[3])
        srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
    log(f"PORT {srv.server_address[1]}")
    srv.serve_forever()


main()

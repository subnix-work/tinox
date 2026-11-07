#!/usr/bin/env python3
"""Bug 463: Autobahn-style WebSocket conformance cases against a tinox.core.websocket echo server.

Raw sockets, so the checks are independent of the implementation under test: masking, RSV bits,
reserved opcodes, control-frame limits, fragmentation, UTF-8 validation (RFC 6455 section 8.1) and
close-frame validation (sections 5.5.1, 7.4). Usage: check.py <port>
"""
import socket, struct, os, base64, sys, time
PORT = int(sys.argv[1])

def connect():
    s = socket.create_connection(("127.0.0.1", PORT), timeout=3)
    key = base64.b64encode(os.urandom(16)).decode()
    s.sendall((f"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
               f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
    buf = b""
    while b"\r\n\r\n" not in buf:
        d = s.recv(1024)
        if not d: raise RuntimeError("closed in handshake")
        buf += d
    assert b" 101 " in buf.split(b"\r\n")[0], buf
    return s

def frame(op, payload=b"", fin=True, mask=True, rsv=0, length_override=None):
    b0 = (0x80 if fin else 0) | (rsv << 4) | op
    n = len(payload)
    m = 0x80 if mask else 0
    if n < 126: hdr = bytes([b0, m | n])
    elif n < 65536: hdr = bytes([b0, m | 126]) + struct.pack(">H", n)
    else: hdr = bytes([b0, m | 127]) + struct.pack(">Q", n)
    if mask:
        k = os.urandom(4)
        return hdr + k + bytes(c ^ k[i % 4] for i, c in enumerate(payload))
    return hdr + payload

def read_frame(s):
    def exact(n):
        d = b""
        while len(d) < n:
            c = s.recv(n - len(d))
            if not c: return None
            d += c
        return d
    h = exact(2)
    if h is None: return None
    op = h[0] & 15; n = h[1] & 127
    if n == 126: n = struct.unpack(">H", exact(2))[0]
    elif n == 127: n = struct.unpack(">Q", exact(8))[0]
    p = exact(n)
    return (op, p)

def expect_close(s, code=None):
    """Read until a close frame or EOF; returns the close code or None."""
    try:
        while True:
            f = read_frame(s)
            if f is None: return "EOF"
            if f[0] == 8:
                return struct.unpack(">H", f[1][:2])[0] if len(f[1]) >= 2 else 0
    except (socket.timeout, ConnectionError):
        return "TIMEOUT"

results = []
def case(name, fn):
    try:
        ok, detail = fn()
    except Exception as e:
        ok, detail = False, f"exception {type(e).__name__}: {e}"
    results.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else "  -> " + str(detail)), flush=True)

def echo(payload, op=1, **kw):
    s = connect(); s.sendall(frame(op, payload, **kw)); f = read_frame(s); s.close(); return f

def t_close(name, data, want, **kw):
    def run():
        s = connect(); s.sendall(data); c = expect_close(s); s.close()
        return (c in want, f"got {c}, want one of {want}")
    case(name, run)

# --- basic echo, sizes
for n in (0, 1, 125, 126, 127, 65535, 65536, 1000000):
    case(f"echo text {n}B", lambda n=n: ((lambda f: (f == (1, b"a" * n), f"got {None if f is None else (f[0], len(f[1]))}"))(echo(b"a" * n))))
case("echo binary all byte values", lambda: ((lambda f: (f == (2, bytes(range(256))), str(f)[:60]))(echo(bytes(range(256)), 2))))
case("echo valid UTF-8 text", lambda: ((lambda f: (f == (1, "héllo €😀".encode()), str(f)[:80]))(echo("héllo €😀".encode()))))
# --- fragmentation
def frag():
    s = connect()
    s.sendall(frame(1, b"Hel", fin=False) + frame(0, b"lo ", fin=False) + frame(0, b"world", fin=True))
    f = read_frame(s); s.close(); return (f == (1, b"Hello world"), f)
case("fragmented text reassembled", frag)
def frag_ping():
    s = connect()
    s.sendall(frame(1, b"ab", fin=False) + frame(9, b"pp") + frame(0, b"cd", fin=True))
    f1 = read_frame(s); f2 = read_frame(s); s.close()
    return (f1 == (10, b"pp") and f2 == (1, b"abcd"), (f1, f2))
case("ping inside fragmented message", frag_ping)
def ping_pong():
    s = connect(); s.sendall(frame(9, b"x" * 125)); f = read_frame(s); s.close(); return (f == (10, b"x" * 125), f)
case("ping 125B answered with pong", ping_pong)
def unsolicited_pong():
    s = connect(); s.sendall(frame(10, b"u") + frame(1, b"after")); f = read_frame(s); s.close(); return (f == (1, b"after"), f)
case("unsolicited pong ignored", unsolicited_pong)
# --- protocol errors expecting 1002 (or connection drop)
P = (1002, "EOF")
t_close("unmasked client frame", frame(1, b"x", mask=False), P)
t_close("RSV1 without extension", frame(1, b"x", rsv=4), P)
t_close("RSV2", frame(1, b"x", rsv=2), P)
t_close("RSV3", frame(1, b"x", rsv=1), P)
t_close("reserved data opcode 3", frame(3, b"x"), P)
t_close("reserved data opcode 7", frame(7, b"x"), P)
t_close("reserved control opcode 11", frame(11, b""), P)
t_close("ping payload 126B", frame(9, b"x" * 126), P)
t_close("fragmented ping", frame(9, b"x", fin=False), P)
t_close("fragmented close", frame(8, b"", fin=False), P)
t_close("continuation without start", frame(0, b"x"), P)
t_close("new data frame inside fragmented message", frame(1, b"a", fin=False) + frame(1, b"b"), P)
t_close("close with 1-byte payload", frame(8, b"\x03"), P)
for code in (0, 999, 1004, 1005, 1006, 1012 - 1 + 0 if False else 1016, 2999, 5000):
    t_close(f"close with invalid code {code}", frame(8, struct.pack(">H", code)), P)
t_close("close with invalid UTF-8 reason", frame(8, struct.pack(">H", 1000) + b"\xff\xfe"), (1007, 1002, "EOF"))
# --- UTF-8 validation (RFC 6455 §8.1: fail the connection with 1007)
U = (1007, "EOF")
t_close("text with invalid UTF-8 byte 0xff", frame(1, b"\xff"), U)
t_close("text with overlong encoding", frame(1, b"\xc0\x80"), U)
t_close("text with surrogate (CESU)", frame(1, b"\xed\xa0\x80"), U)
t_close("text above U+10FFFF", frame(1, b"\xf4\x90\x80\x80"), U)
t_close("text truncated sequence", frame(1, b"\xe2\x82"), U)
t_close("fragmented text, invalid second fragment", frame(1, b"ok", fin=False) + frame(0, b"\xff"), U)
def split_utf8_ok():
    s = connect(); e = "€".encode()
    s.sendall(frame(1, e[:1], fin=False) + frame(0, e[1:], fin=True)); f = read_frame(s); s.close()
    return (f == (1, e), f)
case("UTF-8 character split across fragments is valid", split_utf8_ok)
# --- limits
def oversized():
    s = connect()
    s.sendall(bytes([0x82, 0x80 | 127]) + struct.pack(">Q", 1 << 40) + os.urandom(4))
    c = expect_close(s); s.close(); return (c in (1002, 1009, "EOF"), c)
case("frame declaring 1 TiB is rejected, not allocated", oversized)
def close_echo():
    s = connect(); s.sendall(frame(8, struct.pack(">H", 1000) + b"bye")); c = None
    f = read_frame(s); s.close(); return (f is not None and f[0] == 8 and f[1][:2] == struct.pack(">H", 1000), f)
case("normal close is acknowledged with the same code", close_echo)
fails = [r for r in results if not r[1]]
print(f"\n{len(results) - len(fails)}/{len(results)} passed")
sys.exit(1 if fails else 0)

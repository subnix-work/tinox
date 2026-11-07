#!/usr/bin/env python3
"""Bug 464: HTTP/1 request framing and malformed-request checks (request smuggling vectors, chunked
bodies, Content-Length validation, header syntax, HEAD) against a Tinox HttpServer echo app.
Raw sockets, so they are independent of the implementation under test. Usage: check.py <port>
"""
import socket, sys, time
PORT = int(sys.argv[1])
def send(raw, wait=0.6, read_more=True):
    s = socket.create_connection(("127.0.0.1", PORT), timeout=3)
    s.sendall(raw)
    time.sleep(wait)
    data = b""
    s.settimeout(0.8)
    try:
        while True:
            c = s.recv(65536)
            if not c: break
            data += c
    except (socket.timeout, ConnectionError): pass
    s.close()
    return data
def statuses(data):
    return [int(l.split(b" ")[1]) for l in data.split(b"\r\n") if l.startswith(b"HTTP/1.") and len(l.split(b" ")) > 1]
res = []
def case(name, raw, ok, **kw):
    d = send(raw, **kw)
    st = statuses(d)
    good = ok(st, d)
    res.append(good)
    print(("PASS " if good else "FAIL ") + name + ("" if good else f"  -> statuses={st} {d[:160]!r}"), flush=True)
P = b"POST /echo HTTP/1.1\r\nHost: x\r\n"
one = lambda st, d: len(st) == 1
rej = lambda st, d: (not st) or (st[0] >= 400 and len(st) == 1)   # rejected (4xx/close), never smuggled
# --- baseline
case("plain POST", P + b"Content-Length: 5\r\n\r\nhello", lambda st, d: st == [200] and b"len=5" in d)
case("chunked POST", P + b"Transfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n", lambda st, d: st == [200] and b"len=5 body=hello" in d)
case("pipelined requests answered in order", b"GET /echo HTTP/1.1\r\nHost: a\r\n\r\nGET /echo HTTP/1.1\r\nHost: b\r\n\r\n", lambda st, d: st[:1] == [200] and b"host=a" in d and (len(st) == 1 or d.index(b"host=a") < d.index(b"host=b")))
# --- smuggling vectors: must be rejected or handled unambiguously, never produce a second request from body bytes
smuggled = b"GET /echo HTTP/1.1\r\nHost: evil\r\n\r\n"
case("CL + TE together (CL.TE)", P + b"Content-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n" + b"0\r\n\r\n" + smuggled, lambda st, d: b"host=evil" not in d or st[:1] in ([400], []) )
case("TE + CL together (TE.CL)", P + b"Transfer-Encoding: chunked\r\nContent-Length: 4\r\n\r\n" + b"5c\r\n" + smuggled + b"\r\n0\r\n\r\n", lambda st, d: b"host=evil" not in d)
case("duplicate differing Content-Length", P + b"Content-Length: 5\r\nContent-Length: 6\r\n\r\nhello" + smuggled, lambda st, d: rej(st, d) or b"host=evil" not in d)
case("Content-Length with plus sign", P + b"Content-Length: +5\r\n\r\nhello" + smuggled, lambda st, d: b"host=evil" not in d or st[:1] == [400])
case("Content-Length with leading space value", P + b"Content-Length:  5\r\n\r\nhello", lambda st, d: st == [200])
case("negative Content-Length", P + b"Content-Length: -1\r\n\r\n", rej)
case("non-numeric Content-Length", P + b"Content-Length: abc\r\n\r\n", rej)
case("Transfer-Encoding: chunked, identity", P + b"Transfer-Encoding: chunked, identity\r\n\r\n0\r\n\r\n" + smuggled, lambda st, d: b"host=evil" not in d or rej(st, d))
case("obfuscated TE ('Transfer-Encoding : chunked')", P + b"Content-Length: 4\r\nTransfer-Encoding : chunked\r\n\r\n" + b"0\r\n\r\n" + smuggled, lambda st, d: rej(st, d) or b"host=evil" not in d)
case("invalid chunk size", P + b"Transfer-Encoding: chunked\r\n\r\nzz\r\nhello\r\n0\r\n\r\n", rej)
case("chunk size overflow", P + b"Transfer-Encoding: chunked\r\n\r\nffffffffffffffffff\r\nx\r\n0\r\n\r\n", rej)
case("chunk extension is ignored", P + b"Transfer-Encoding: chunked\r\n\r\n5;ext=1\r\nhello\r\n0\r\n\r\n", lambda st, d: st == [200] and b"len=5" in d)
# --- header / request line abuse
case("bare LF line endings", b"GET /echo HTTP/1.1\nHost: x\n\n", lambda st, d: st in ([200], [400]) or not st)
case("header with NUL byte", b"GET /echo HTTP/1.1\r\nHost: x\r\nX-Test: a\x00b\r\n\r\n", rej)
case("header name with space", b"GET /echo HTTP/1.1\r\nHost: x\r\nBad Name: v\r\n\r\n", rej)
case("obs-fold continuation line", b"GET /echo HTTP/1.1\r\nHost: x\r\nX-Test: a\r\n b\r\n\r\n", lambda st, d: st in ([400], []) or (st == [200]))
case("request line with two spaces", b"GET  /echo HTTP/1.1\r\nHost: x\r\n\r\n", lambda st, d: st in ([400], [200], [404]) or not st)
case("missing Host on HTTP/1.1", b"GET /echo HTTP/1.1\r\n\r\n", lambda st, d: st[:1] in ([400], [200]) )
case("unknown HTTP version", b"GET /echo HTTP/9.9\r\nHost: x\r\n\r\n", lambda st, d: st[:1] in ([400], [505], []))
case("garbage request", b"\x16\x03\x01\x00\xa5\x01\x00\x00\xa1", lambda st, d: not st or st[0] >= 400)
case("very long request line (64 KB)", b"GET /" + b"a" * 65536 + b" HTTP/1.1\r\nHost: x\r\n\r\n", lambda st, d: not st or st[0] in (400, 404, 414, 431))
case("2000 header fields", b"GET /echo HTTP/1.1\r\nHost: x\r\n" + b"".join(b"X-%d: v\r\n" % i for i in range(2000)) + b"\r\n", lambda st, d: not st or st[0] in (200, 400, 431))
case("huge Content-Length, no body (no allocation/hang)", P + b"Content-Length: 99999999999\r\n\r\n", lambda st, d: not st or st[0] >= 400, wait=1.0)
case("body slower than declared (short body)", P + b"Content-Length: 50\r\n\r\nshort", lambda st, d: not st or st[0] >= 400, wait=1.0)
case("HEAD request has no body", b"HEAD /echo HTTP/1.1\r\nHost: x\r\n\r\n", lambda st, d: st[:1] in ([200], [404], [405]) and not d.split(b"\r\n\r\n", 1)[1])
case("path traversal is not served from disk", b"GET /../../../../etc/passwd HTTP/1.1\r\nHost: x\r\n\r\n", lambda st, d: b"root:" not in d)
case("encoded traversal", b"GET /%2e%2e/%2e%2e/etc/passwd HTTP/1.1\r\nHost: x\r\n\r\n", lambda st, d: b"root:" not in d)
case("absolute-form target", b"GET http://x/echo HTTP/1.1\r\nHost: x\r\n\r\n", lambda st, d: len(st) == 1)
case("Expect: 100-continue", P + b"Expect: 100-continue\r\nContent-Length: 5\r\n\r\nhello", lambda st, d: 200 in st)
case("HTTP/1.0 request closes", b"GET /echo HTTP/1.0\r\n\r\n", lambda st, d: st[:1] in ([200], [400]))
bad = res.count(False)
print(f"\n{len(res) - bad}/{len(res)} passed")
sys.exit(1 if bad else 0)

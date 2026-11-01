"""Workspace HTTP/2 and HTTP/3 binary bridges against independent curl."""
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time

def h2_echo(port, payload, invalid_frame_size=False):
    # Independent RFC 7540 frame client using the server's documented
    # non-Huffman HPACK subset. curl's Huffman headers are unsupported.
    def frame(kind, flags, stream, data):
        return len(data).to_bytes(3, 'big') + bytes([kind, flags]) + stream.to_bytes(4, 'big') + data
    def exact(sock, count):
        data = bytearray()
        while len(data) < count:
            chunk = sock.recv(count - len(data))
            if not chunk:
                raise RuntimeError('HTTP/2 early EOF')
            data.extend(chunk)
        return bytes(data)
    with socket.create_connection(('127.0.0.1', port), timeout=4) as sock:
        # Indexed :method POST and :scheme http; literal :path /echo.
        headers = b'\x83\x86\x04\x05/echo'
        data_frames = b''.join(frame(0, int(offset + 16384 >= len(payload)), 1, payload[offset:offset + 16384])
                               for offset in range(0, len(payload), 16384))
        sock.sendall(b'PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n' + frame(4, 0, 0, b'\x00\x05\x00\x00\x00\x00' if invalid_frame_size else b'') +
                     frame(1, 4, 1, headers) + data_frames)
        output = bytearray()
        status_ok = False
        while True:
            head = exact(sock, 9)
            data = exact(sock, int.from_bytes(head[:3], 'big'))
            kind, flags = head[3:5]
            if kind == 4 and not flags & 1:
                sock.sendall(frame(4, 1, 0, b''))
            if kind == 7 and invalid_frame_size:
                assert int.from_bytes(data[4:8], 'big') == 1
                return b'goaway'
            if kind == 1:
                status_ok = data.startswith(b'\x88')  # static :status 200
            if kind == 0:
                assert len(data) <= 16384
                output.extend(data)
            if kind in (0, 1) and flags & 1:
                assert status_ok
                return bytes(output)


tinox, repo = map(Path, sys.argv[1:])
for protocol in (2, 3):
    with tempfile.TemporaryDirectory(prefix=f'tinox-bytes-h{protocol}-') as directory:
        work = Path(directory)
        for module in ('http_server', 'http2_server', 'hpack', 'http3_server', 'json', 'amqp091'):
            shutil.copytree(repo / f'crates/tinox-core-ext/{module}/tinox',
                            work / f'.tinox/deps/tinox.core/{module}/checkout/tinox')
        (work / 'tinox.toml').write_text('[package]\ncompiler="2.9.0"\nname="protocol_bytes"\nversion="0.0.0"\n')
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM if protocol == 3 else socket.SOCK_STREAM) as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        server = (f'Http2Server::new({port})' if protocol == 2 else
                  f'Http3Server::new({port}, "{repo}/tests/fixtures/tls/selfsigned_cert.pem", "{repo}/tests/fixtures/tls/selfsigned_key.pem")')
        source = '''import tinox.core.http_server
import tinox.core.httpPROTOCOL_server
import tinox.core.bytes
import tinox.core.amqp091
class Main {
 fnc main() -> Int32 {
  const server = SERVER
  server.post("/echo", ctx => {
   const bytes = Bytes::fromRequestBody(ctx.request.body)
   // Messaging legacy adapters explicitly copy in both directions.
   const message = AmqpMessage091 { body: [], deliveryTag: 0, exchange: "", routingKey: "", contentType: "", ok: true, errorMessage: "", redelivered: false, messageId: "" }
   message.setBodyBytes(bytes)
   const snapshot = message.bodyBytes()
   if (bytes.len() > 0) { bytes.set(0, 33) }
   ctx.response.body = snapshot.asResponseBody()
   gcCollect()
   ctx.response.header("Content-Type", "application/octet-stream")
  })
  server.listen()
  return 0
 }
}
'''.replace('PROTOCOL', str(protocol)).replace('SERVER', server)
        (work / 'Main.tnx').write_text(source)
        env = dict(os.environ, TINOX_HTTP3=str(int(protocol == 3)))
        build = subprocess.run([tinox, 'build', 'Main.tnx', '-o', work / 'service'],
                               cwd=work, env=env, capture_output=True, timeout=90)
        assert build.returncode == 0, build.stderr.decode()
        service = subprocess.Popen([work / 'service'], cwd=work, stdout=(work / 'service.log').open('wb'), stderr=subprocess.PIPE)
        args = ['curl', '--http2-prior-knowledge' if protocol == 2 else '--http3-only', '-ksS',
                '--max-time', '4', '-H', 'Content-Type: application/octet-stream',
                '--data-binary', '@-', f'{"http" if protocol == 2 else "https"}://127.0.0.1:{port}/echo']
        try:
            # Each protocol gets 15 full request round trips, not Tinox self-tests.
            for repeat in range(15):
                payload = bytes(range(256)) * (128 if repeat == 14 else 1 + repeat % 4)
                deadline = time.monotonic() + 10
                while True:
                    try:
                        if protocol == 2:
                            actual = h2_echo(port, payload)
                        else:
                            result = subprocess.run(args, input=payload, capture_output=True, timeout=6)
                            if result.returncode:
                                raise RuntimeError(result.stderr)
                            actual = result.stdout
                        break
                    except (OSError, RuntimeError):
                        if service.poll() is not None or time.monotonic() > deadline:
                            raise
                        time.sleep(.05)
                assert actual == payload, (protocol, repeat, len(actual))
                if protocol == 2:
                    assert h2_echo(port, payload, invalid_frame_size=True) == b'goaway'
        finally:
            service.kill()
            _, error = service.communicate(timeout=5)
            if error: print(error.decode(errors="replace"), file=sys.stderr)
print('30 independent HTTP/2 (non-Huffman HPACK) and HTTP/3 binary round trips passed')

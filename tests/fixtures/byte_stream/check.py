"""Independent Python socket/TLS and gzip checks; no Tinox client oracle."""
import gzip
import os
from pathlib import Path
import selectors
import socket
import ssl
import subprocess
import sys
import tempfile
import time

exe, repo = map(Path, sys.argv[1:])
payload = bytes(range(256)) * 1024
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    (root / 'input').write_bytes(payload)
    env = dict(os.environ, TINOX_STREAM_MODE='codec', TINOX_STREAM_INPUT=str(root / 'input'),
               TINOX_STREAM_OUTPUT=str(root / 'compressed'))
    result = subprocess.run([exe], env=env, capture_output=True, timeout=10)
    assert result.returncode == 0, result.stderr
    assert gzip.decompress((root / 'compressed').read_bytes()) == payload

for tls in (False, True):
    for mode in ('echo', 'http', 'eof', 'invalid', 'timeout', 'cancel', 'blocked-write'):
        for repeat in range(15):
            env = dict(os.environ, TINOX_STREAM_MODE=mode, TINOX_STREAM_TLS=str(int(tls)),
                       TINOX_STREAM_SIZE=str(len(payload)),
                       TINOX_STREAM_CERT=str(repo / 'tests/fixtures/tls/selfsigned_cert.pem'),
                       TINOX_STREAM_KEY=str(repo / 'tests/fixtures/tls/selfsigned_key.pem'))
            process = subprocess.Popen([exe], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    assert selector.select(5), 'server readiness timeout'
                port = int(process.stdout.readline())
                sock = socket.create_connection(('127.0.0.1', port), timeout=4)
                if tls:
                    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
                    context.check_hostname = False
                    context.verify_mode = ssl.CERT_NONE
                    sock = context.wrap_socket(sock, server_hostname='localhost')
                with sock:
                    if mode == 'eof':
                        if tls:
                            sock.unwrap().close()
                        else:
                            sock.shutdown(socket.SHUT_WR)
                            assert sock.recv(1) == b''
                    elif mode == 'invalid':
                        assert sock.recv(1) == b''
                    elif mode == 'http':
                        import http.client
                        sock.sendall(b'POST /stream HTTP/1.1\r\nHost: localhost\r\nContent-Length: ' + str(len(payload)).encode() + b'\r\n\r\n' + payload)
                        response = http.client.HTTPResponse(sock)
                        response.begin()
                        assert response.status == 200
                        assert response.read() == payload
                    elif mode == 'echo':
                        output = bytearray()
                        for start in range(0, len(payload), 3071):
                            part = payload[start:start + 3071]
                            sock.sendall(part)
                            got = bytearray()
                            while len(got) < len(part):
                                chunk = sock.recv(len(part) - len(got))
                                assert chunk, 'early EOF'
                                got.extend(chunk)
                            output.extend(got)
                        assert output == payload
                    else:
                        # Do not read during the write test: force bounded wait.
                        time.sleep(.3)
                output, error = process.communicate(timeout=5)
                assert process.returncode == 0, (tls, mode, repeat, output, error)
                if mode == 'eof':
                    assert output.strip() == b'eof'
                elif mode == 'invalid':
                    assert output.strip() == b'22'
                elif mode == 'http':
                    assert output.strip() == b''
                elif mode == 'echo':
                    assert output.strip() == str(len(payload)).encode()
                elif mode == 'timeout':
                    assert output.strip() == b'110'  # Linux ETIMEDOUT
                elif mode == 'cancel':
                    assert output.strip() == b'125'  # Linux ECANCELED
                else:
                    assert output.strip() == b'backpressure timeout'
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait()


# Rejected framing must close the owned transport instead of consuming an
# ambiguous body. Each malformed case is exercised repeatedly over TCP/TLS.
bad_headers = [
    b'POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx',
    b'POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n',
    b'POST / HTTP/1.1\r\nContent-Length: 9223372036854775808\r\n\r\n',
    b'POST / HTTP/1.1\r\nContent-Length: -1\r\n\r\n',
    b'POST / HTTP/1.1\r\nX-Nul: a\x00b\r\n\r\n',
    b'POST / HTTP/1.1\r\nX-Long: ' + b'a' * 16384 + b'\r\n\r\n',
]
for tls in (False, True):
    for header in bad_headers:
        for repeat in range(15):
            env = dict(os.environ, TINOX_STREAM_MODE='http', TINOX_STREAM_TLS=str(int(tls)),
                       TINOX_STREAM_CERT=str(repo / 'tests/fixtures/tls/selfsigned_cert.pem'),
                       TINOX_STREAM_KEY=str(repo / 'tests/fixtures/tls/selfsigned_key.pem'))
            process = subprocess.Popen([exe], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    assert selector.select(5)
                port = int(process.stdout.readline())
                sock = socket.create_connection(('127.0.0.1', port), timeout=3)
                if tls:
                    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
                    context.check_hostname = False
                    context.verify_mode = ssl.CERT_NONE
                    sock = context.wrap_socket(sock, server_hostname='localhost')
                with sock:
                    try:
                        sock.sendall(header)
                        assert sock.recv(1) == b''
                    except (ConnectionResetError, BrokenPipeError, ssl.SSLError):
                        pass
                output, error = process.communicate(timeout=5)
                assert process.returncode == 1, (tls, header[:50], output, error)
                assert b'Uncaught error:' in error
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait()
print('210 stream trials, 180 malformed-header trials and independent gzip checks passed')

"""Independent protocol and SQL observers for the composed managed service."""
import concurrent.futures
import http.client
import http.server
import json
import os
import signal
import socket
import subprocess
import sys
import threading
import time
from service_lifecycle import line

PROGRAM, DATABASE = sys.argv[1:]


def sql(query):
    result = subprocess.run(["psql", DATABASE, "-tA", "-v", "ON_ERROR_STOP=1", "-c", query],
                            capture_output=True, timeout=5)
    assert result.returncode == 0, result.stderr
    return result.stdout.decode().strip()


def exact(connection, count):
    data = b""
    while len(data) < count:
        chunk = connection.recv(count - len(data))
        assert chunk, "premature WebSocket close"
        data += chunk
    return data


def frame(connection):
    header = exact(connection, 2)
    assert header[1] < 126, header
    return header[0] & 15, exact(connection, header[1])


class Dependency(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.server.entered.set()
        assert self.server.release.wait(3), "client never initiated service draining"
        body = b"dependent work completed"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


for iteration in range(20):
    dependency = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Dependency)
    dependency.entered, dependency.release = threading.Event(), threading.Event()
    dependency_thread = threading.Thread(target=dependency.serve_forever)
    dependency_thread.start()
    # Keep the reserved socket bound but never listen: refusal is deterministic
    # and no other process can claim the chosen dynamic port.
    refusing = socket.socket()
    refusing.bind(("127.0.0.1", 0))
    process = subprocess.Popen([PROGRAM], cwd=os.path.dirname(PROGRAM), stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, env={**os.environ, "TINOX_HTTP_WORKERS": "4",
                               "COMPOSITION_REFUSED": f"http://127.0.0.1:{refusing.getsockname()[1]}/",
                               "COMPOSITION_DEPENDENCY": f"http://127.0.0.1:{dependency.server_port}/slow"})
    try:
        ports = line(process).split(":")
        assert ports[0] == "ports", ports
        # Listener order is a compiler implementation detail; identify HTTP
        # through its independent health response instead of hardcoding it.
        ports = [int(port) for port in ports[1:]]

        def request(port, path):
            client = http.client.HTTPConnection("127.0.0.1", port, timeout=4)
            try:
                client.request("GET", path)
                response = client.getresponse()
                return response.status, response.read().decode()
            finally:
                client.close()

        http_port = None
        for port in ports:
            status, body = request(port, "/health/live")
            if status == 200:
                http_port = port
                break
        assert http_port is not None
        ws_port = next(port for port in ports if port != http_port)
        with socket.create_connection(("127.0.0.1", ws_port), timeout=4) as ws:
            ws.sendall(b"GET /counter HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n")
            response = b""
            while not response.endswith(b"\r\n\r\n"):
                response += exact(ws, 1)
            assert b"101 Switching Protocols" in response, response
            with concurrent.futures.ThreadPoolExecutor(8) as pool:
                pending = [pool.submit(request, http_port, "/add") for _ in range(8)]
                mask, payload = b"\x01\x02\x03\x04", b"add"
                ws.sendall(b"\x81\x83" + mask + bytes(value ^ mask[i % 4] for i, value in enumerate(payload)))
                opcode, payload = frame(ws)
                values = [int(payload)]
                for future in pending:
                    status, body = future.result()
                    assert status == 200, body
                    values.append(json.loads(body))
                assert sorted(values) == list(range(1, 10)), values
            assert request(http_port, "/refused") == (200, '"dependency unavailable"')
            before = int(sql("SELECT balance FROM accounts WHERE id=1"))
            assert request(http_port, "/commit") == (200, str(before + 1))
            assert request(http_port, "/rollback") == (200, '"rollback requested"')
            baseline_threads = len(os.listdir(f"/proc/{process.pid}/task"))
            assert request(http_port, "/cancel") == (200, '"Task cancelled"')
            for _ in range(100):
                remaining_threads = len(os.listdir(f"/proc/{process.pid}/task"))
                if remaining_threads <= baseline_threads:
                    break
                time.sleep(.01)
            assert remaining_threads <= baseline_threads, ("cancelled child survived join", baseline_threads, remaining_threads)
            assert sql("SELECT balance FROM accounts WHERE id=1") == str(before + 1)
            # With a pool of two, transactions above must release their lease:
            # no backend may remain idle in a transaction before shutdown.
            assert sql("SELECT count(*) FROM pg_stat_activity WHERE state LIKE 'idle in transaction%'") == "0"
            with concurrent.futures.ThreadPoolExecutor(1) as pool:
                outbound = pool.submit(request, http_port, "/outbound")
                assert dependency.entered.wait(3)
                assert sql("SELECT count(*) FROM pg_stat_activity WHERE state='idle in transaction'") == "1"
                start = time.monotonic()
                process.send_signal(signal.SIGTERM if iteration % 2 else signal.SIGINT)
                dependency.release.set()
                assert outbound.result() == (200, '"dependency rollback"')
                assert sql("SELECT balance FROM accounts WHERE id=1") == str(before + 1)
                opcode, payload = frame(ws)
                assert opcode == 8 and payload[:2] == b"\x03\xe9", (opcode, payload)
            assert process.wait(timeout=4) == 0
            output, errors = process.communicate()
            assert output.decode().splitlines() == ["websocket-closed", "disposed:9"], output
            assert not errors, errors
            assert time.monotonic() - start < 3
        for port in ports:
            with socket.socket() as probe:
                probe.settimeout(.3)
                assert probe.connect_ex(("127.0.0.1", port)) != 0, ("leaked listener", port)
        assert sql("SELECT count(*) FROM pg_stat_activity WHERE backend_type='client backend' AND pid<>pg_backend_pid()") == "0"
        assert not os.path.exists(f"/proc/{process.pid}/task"), "service threads survived process exit"
        print(f"iteration {iteration + 1}: shared=9 committed={before + 1} cancelled/rolled-back/drained/disposed", flush=True)
    except BaseException:
        exit_status = process.poll()
        if exit_status is None:
            process.kill()
        output, errors = process.communicate()
        print("iteration", iteration, "service status", exit_status, "stdout", output, "stderr", errors, file=sys.stderr)
        raise
    finally:
        refusing.close()
        dependency.release.set()
        dependency.shutdown()
        dependency.server_close()
        dependency_thread.join(timeout=3)
        if process.poll() is None:
            process.kill()
            process.wait()

"""Independent HTTP/WebSocket clients for generated service lifecycle wiring."""
import concurrent.futures
import http.client
import json
import os
import select
import signal
import socket
import subprocess
import sys
import time


def line(process):
    # stdout is unbuffered binary so select observes every printed event.
    data = bytearray()
    while not data.endswith(b"\n"):
        assert select.select([process.stdout], [], [], 5)[0], "missing service event"
        chunk = os.read(process.stdout.fileno(), 1)
        assert chunk, (data, process.poll(), process.stderr.read())
        data.extend(chunk)
    return data.decode().strip()


def launch(program):
    return subprocess.Popen([program], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env={**os.environ, "TINOX_PORT": "0", "TINOX_HTTP_WORKERS": "4"}, cwd=os.path.dirname(program))


def finish(process, expected=0):
    assert process.wait(timeout=5) == expected
    output, errors = process.communicate()
    if expected == 0:
        assert not errors, errors
    return output.decode(), errors.decode()


def check_http(program):
    observed_draining = 0
    for iteration in range(20):
        process = launch(program)
        try:
            assert line(process) == "init"
            port = int(line(process))

            def request(path, method="GET"):
                client = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
                client.request(method, path)
                response = client.getresponse()
                result = response.status, response.read().decode()
                client.close()
                return result

            assert request("/health/ready")[0] == 503
            assert request("/health/live")[0] == 200
            assert request("/enable", "POST") == (200, '"enabled"')
            for _ in range(100):
                status, body = request("/health/ready")
                if status == 200:
                    break
                time.sleep(.01)
            assert status == 200, body
            state = json.loads(body)
            assert 0 < state["startedMs"] <= state["readyMs"]
            assert state["shutdownMs"] == 0
            with concurrent.futures.ThreadPoolExecutor(1) as executor:
                pending = executor.submit(request, "/slow")
                assert line(process) == "handling"
                signalled_at = time.monotonic()
                process.send_signal(signal.SIGTERM if iteration % 2 else signal.SIGINT)
                time.sleep(.03)
                try:
                    status, body = request("/health/ready")
                    state = json.loads(body)
                    assert status == 503 and state["state"] == "draining", state
                    assert state["shutdownMs"] >= state["readyMs"]
                    observed_draining += 1
                    assert request("/fast") == (503, "Service draining")
                except (ConnectionResetError, ConnectionRefusedError, http.client.RemoteDisconnected):
                    # SO_REUSEPORT can assign a new probe to the worker executing
                    # /slow. That probe can outlive the drain and see the socket
                    # close; the admitted response must still finish successfully.
                    assert time.monotonic() - signalled_at >= .4
                assert pending.result(timeout=3) == (200, '"complete"')
            assert finish(process)[0] == "stop\n"
        except BaseException:
            process.kill()
            output, errors = process.communicate()
            print("iteration", iteration, "stdout", output, "stderr", errors, file=sys.stderr)
            raise
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()

    assert observed_draining > 0, "no live draining health response observed"


def websocket(program):
    def read_exact(connection, count):
        result = b""
        while len(result) < count:
            chunk = connection.recv(count - len(result))
            assert chunk, "WebSocket closed before its final frames"
            result += chunk
        return result

    def frame(connection):
        head = read_exact(connection, 2)
        assert head[1] < 126
        return head[0] & 15, read_exact(connection, head[1])

    for iteration in range(20):
        process = launch(program)
        try:
            port = int(line(process))
            with socket.create_connection(("127.0.0.1", port), timeout=3) as client:
                client.sendall(b"GET /echo HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n")
                response = b""
                while not response.endswith(b"\r\n\r\n"):
                    response += read_exact(client, 1)
                assert b"101 Switching Protocols" in response
                assert b"s3pPLMBiTxaQ9kYGzzhZRbK+xOo=" in response
                # A valid masked client frame, independent of the tinox codec.
                client.sendall(b"\x81\x84\x01\x02\x03\x04" + bytes(a ^ b for a, b in zip(b"work", b"\x01\x02\x03\x04")))
                assert line(process) == "handling"
                process.send_signal(signal.SIGTERM if iteration % 2 else signal.SIGINT)
                assert frame(client) == (1, b"work")
                opcode, payload = frame(client)
                assert opcode == 8 and payload[:2] == b"\x03\xe9", (opcode, payload)
            assert finish(process)[0] == "closed\nstop\n"
        except BaseException:
            process.kill()
            output, errors = process.communicate()
            print("iteration", iteration, "stdout", output, "stderr", errors, file=sys.stderr)
            raise
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()



def http3(program):
    for iteration in range(20):
        process = launch(program)
        try:
            port = int(line(process))
            url = f"https://127.0.0.1:{port}"
            args = ["curl", "--http3-only", "-ksS", "--max-time", "3"]
            for endpoint in ["/health/ready", "/health/live"]:
                for _ in range(100):
                    response = subprocess.run(args + ["-w", "\\n%{http_code}", url + endpoint], capture_output=True)
                    if response.stdout.endswith(b"\n200"):
                        break
                    time.sleep(.01)
                assert response.returncode == 0 and response.stdout.endswith(b"\n200"), response
            client = subprocess.Popen(args + [url + "/slow"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            assert line(process) == "handling"
            process.send_signal(signal.SIGTERM if iteration % 2 else signal.SIGINT)
            output, errors = client.communicate(timeout=4)
            assert client.returncode == 0 and output == b'"complete"', (output, errors)
            assert finish(process)[0] == ""
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()


if __name__ == "__main__":
    {"http": check_http, "websocket": websocket, "http3": http3}[sys.argv[1]](sys.argv[2])

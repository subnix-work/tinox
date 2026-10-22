#!/usr/bin/env python3
"""Independent HTTP interoperability, isolation, bounded export and timing checks.

Run directly or via cargo test --release -p tinox --test observability.
Requires Python 3, a C compiler, pthreads and zlib, with no Python packages.
"""
import contextlib
import http.client
import http.server
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
PARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
TRACE = PARENT[3:35]


class Peer(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.server.received.append(dict(self.headers))
        self.send_response(200)
        self.send_header("Content-Length", "2")
        self.end_headers()
        self.wfile.write(b"ok")

    def do_POST(self):
        assert self.path == "/v1/traces"
        assert self.headers["Content-Type"] == "application/json"
        record = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.received.append(record)
        if self.server.stall:
            time.sleep(0.6)
        self.send_response(200)
        self.send_header("Content-Length", "2")
        self.end_headers()
        with contextlib.suppress(BrokenPipeError, ConnectionResetError):
            self.wfile.write(b"{}")


@contextlib.contextmanager
def peer(stall=False):
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Peer)
    server.received, server.stall = [], stall
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def endpoint(server):
    return f"http://127.0.0.1:{server.server_port}"


def run_checks(binary, workdir):
    base = {k: v for k, v in os.environ.items() if not k.startswith("TINOX_TELEMETRY_")}
    base["TINOX_TELEMETRY_ENABLED"] = "true"

    def run(*args, env=None, success=True):
        result = subprocess.run([str(binary), *args], env=env or base, cwd=workdir,
                                capture_output=True, text=True, timeout=10)
        assert result.returncode == 0 if success else result.returncode != 0, result.stderr
        return result

    output = run("contexts").stdout.splitlines()
    records = [json.loads(line) for line in output if line.startswith("{")]
    assert records[0]["message"] == 'quote" slash\\ line\n tab\t control\x01'
    assert records[0]["trace_id"] == TRACE
    assert records[0]["request_id"] == "request-1"
    assert len(records[1]["message"]) == 4096
    assert "contexts ok" in output
    for key, value in (("ENABLED", "yes"), ("SAMPLE", "10001"), ("SAMPLE", "oops"),
                       ("SERVICE", "secret\nservice"), ("ENDPOINT", "http://localhost:4318/v1/traces")):
        assert "invalid telemetry configuration" in run("bench", env={**base, "TINOX_TELEMETRY_" + key: value}, success=False).stderr
    # Exercise the shared properties path, then verify environment precedence.
    properties = Path(workdir) / "application.properties"
    properties.write_text("tinox.telemetry.enabled=true\ntinox.telemetry.sample=0\n")
    no_env = {k: v for k, v in base.items() if not k.startswith("TINOX_TELEMETRY_")}
    assert "enabled=1" in run("bench", env=no_env).stdout
    assert "enabled=0" in run("bench", env={**no_env, "TINOX_TELEMETRY_ENABLED": "false"}).stdout
    properties.unlink()

    with peer() as dependency, peer() as collector:
        env = {**base, "TINOX_TELEMETRY_ENDPOINT": endpoint(collector) + "/v1/traces"}
        process = subprocess.Popen([str(binary), "server", endpoint(dependency), "23"],
                                   env=env, cwd=workdir, text=True, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE)
        log_lines = []
        reader = None
        try:
            # Startup is bounded by the enclosing test subprocess timeout.
            port_line = process.stdout.readline().strip()
            assert port_line.startswith("PORT "), port_line
            reader = threading.Thread(target=lambda: log_lines.extend(process.stdout), daemon=True)
            reader.start()
            port = int(port_line.split()[1])
            ids = []
            for i in range(23):
                client = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
                client.putrequest("GET", "/probe?secret=never-capture")
                if i < 20:
                    client.putheader("traceparent", PARENT)
                    client.putheader("x-request-id", f"client-{i}")
                elif i == 20:
                    client.putheader("traceparent", "malformed")
                    client.putheader("x-request-id", "x" * 65)
                elif i == 21:
                    client.putheader("traceparent", PARENT)
                    client.putheader("traceparent", PARENT)
                client.putheader("Authorization", "Bearer never-capture")
                client.endheaders()
                response = client.getresponse()
                assert response.status == 200
                assert response.read() == b"ok"
                request_id = response.getheader("X-Request-Id")
                assert request_id == f"client-{i}" if i < 20 else re.fullmatch("[0-9a-f]{32}", request_id)
                ids.append(request_id)
                client.close()
            process.wait(timeout=5)
            reader.join(timeout=1)
            assert not reader.is_alive()
            assert process.returncode == 0, process.stderr.read()
            stdout = "".join(log_lines)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            if reader is not None:
                reader.join(timeout=1)
        logs = [json.loads(line) for line in stdout.splitlines() if line.startswith("{")]
        assert len(logs) == 23 * 4
        assert "never-capture" not in stdout
        assert len(dependency.received) == 23
        spans = []
        for batch in collector.received:
            resource = batch["resourceSpans"][0]
            assert resource["resource"]["attributes"][0]["value"]["stringValue"] == "tinox"
            spans.extend(resource["scopeSpans"][0]["spans"])
        assert len(spans) == 23 * 3, len(spans)
        by_span = {s["spanId"]: s for s in spans}
        for span in spans:
            assert re.fullmatch("[0-9a-f]{32}", span["traceId"])
            assert re.fullmatch("[0-9a-f]{16}", span["spanId"])
            assert int(span["endTimeUnixNano"]) >= int(span["startTimeUnixNano"])
            assert span["name"] in ("http.server", "http.client", "task")
            assert isinstance(span["kind"], int)
        for i, headers in enumerate(dependency.received):
            outgoing = headers["traceparent"]
            assert re.fullmatch("00-[0-9a-f]{32}-[0-9a-f]{16}-01", outgoing)
            assert headers["x-request-id"] == ids[i]
            call = by_span[outgoing[36:52]]
            task = by_span[call["parentSpanId"]]
            server = by_span[task["parentSpanId"]]
            assert (call["kind"], task["kind"], server["kind"]) == (3, 1, 2)
            assert call["traceId"] == task["traceId"] == server["traceId"]
            if i < 20:
                assert call["traceId"] == TRACE and server["parentSpanId"] == PARENT[36:52]
            else:
                assert call["traceId"] != TRACE and not server["parentSpanId"]
            request_logs = [log for log in logs if log["request_id"] == ids[i]]
            assert len(request_logs) == 4
            assert {log["trace_id"] for log in request_logs} == {call["traceId"]}
            assert {log["span_id"] for log in request_logs} == {server["spanId"], task["spanId"]}

    with peer(stall=True) as collector:
        env = {**base, "TINOX_TELEMETRY_ENDPOINT": endpoint(collector) + "/v1/traces"}
        for _ in range(15):
            result = run("flood", env=env)
            assert "tinox_telemetry_dropped_total" in result.stdout
    # Connection refusal must also be bounded. Reserve then close a dynamic port.
    import socket
    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        refused = reserved.getsockname()[1]
    run("flood", env={**base, "TINOX_TELEMETRY_ENDPOINT": f"http://127.0.0.1:{refused}/v1/traces"})
    with peer() as collector:
        env = {**base, "TINOX_TELEMETRY_SAMPLE": "0", "TINOX_TELEMETRY_ENDPOINT": endpoint(collector) + "/v1/traces"}
        run("flood", env=env)
        assert not collector.received
    for enabled in ("false", "true"):
        print(run("bench", env={**base, "TINOX_TELEMETRY_ENABLED": enabled}).stdout.strip())
    print("observability: 23 request/task/dependency chains, OTLP JSON and bounded exporter checks passed")


def main():
    with tempfile.TemporaryDirectory(prefix="tinox-observability-") as workdir:
        binary = Path(workdir) / "observability"
        subprocess.run([os.environ.get("CC", "cc"), "-O2", "-Wall", "-Wextra",
                        str(ROOT / "tests/support/observability.c"), "-o", str(binary),
                        "-pthread", "-lm", "-lz", "-lutil"], check=True)
        run_checks(binary, workdir)


if __name__ == "__main__":
    main()

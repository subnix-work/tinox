#!/usr/bin/env python3
"""Issue #548: the runtime's OTLP/HTTP JSON export against the OFFICIAL OpenTelemetry Collector.

Starts otel/opentelemetry-collector (OTLP/HTTP receiver, `debug` exporter with detailed output) in
Docker, drives the same C harness as check-observability.py through 23 request/task/outbound-call
chains, and rebuilds the span graph from what the Collector itself decoded and printed. Exits 0 with
a SKIP line when Docker is unavailable.
"""
import http.client
import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("obs", ROOT / "scripts/check-observability.py")
obs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(obs)

CONFIG = """receivers:
  otlp:
    protocols:
      http:
        endpoint: 0.0.0.0:4318
exporters:
  debug:
    verbosity: detailed
service:
  telemetry:
    metrics:
      level: none
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [debug]
"""
IMAGE = os.environ.get("TINOX_OTEL_COLLECTOR_IMAGE", "otel/opentelemetry-collector:latest")


def parse_spans(log):
    spans = []
    for block in log.split("Span #")[1:]:
        def field(name):
            m = re.search(r"^\s*" + name + r"\s*:[ \t]*(.*)$", block, re.M)
            return m.group(1).strip() if m else None
        spans.append({"trace": field("Trace ID"), "parent": field("Parent ID") or "", "id": field("ID"),
                      "name": field("Name"), "kind": field("Kind")})
    return spans


def main():
    if shutil.which("docker") is None:
        print("SKIP: docker not installed")
        return
    name = f"tinox-otel-{os.getpid()}"
    with tempfile.TemporaryDirectory(prefix="tinox-otel-") as work:
        config = Path(work) / "config.yaml"
        config.write_text(CONFIG)
        os.chmod(work, 0o755)
        os.chmod(config, 0o644)
        binary = Path(work) / "observability"
        subprocess.run([os.environ.get("CC", "cc"), "-O2", str(ROOT / "tests/support/observability.c"), "-o", str(binary),
                        "-pthread", "-lm", "-lz", "-lutil"], check=True)
        subprocess.run(["docker", "run", "-d", "--name", name, "-p", "127.0.0.1:0:4318",
                        "-v", f"{config}:/etc/otelcol/config.yaml:ro", IMAGE, "--config", "/etc/otelcol/config.yaml"],
                       check=True, capture_output=True)
        try:
            port = None
            for _ in range(60):
                out = subprocess.run(["docker", "port", name, "4318/tcp"], capture_output=True, text=True).stdout.strip()
                if out:
                    port = int(out.splitlines()[0].rsplit(":", 1)[1])
                    if "Everything is ready" in subprocess.run(["docker", "logs", name], capture_output=True, text=True).stderr:
                        break
                time.sleep(0.5)
            else:
                raise AssertionError("collector did not become ready")
            base = {k: v for k, v in os.environ.items() if not k.startswith("TINOX_TELEMETRY_")}
            base["TINOX_TELEMETRY_ENABLED"] = "true"
            base["TINOX_TELEMETRY_ENDPOINT"] = f"http://127.0.0.1:{port}/v1/traces"
            with obs.peer() as dependency:
                process = subprocess.Popen([str(binary), "server", obs.endpoint(dependency), "23"], env=base, cwd=work,
                                           text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                port_line = process.stdout.readline().strip()
                assert port_line.startswith("PORT "), port_line
                app = int(port_line.split()[1])
                for i in range(23):
                    client = http.client.HTTPConnection("127.0.0.1", app, timeout=3)
                    client.putrequest("GET", "/probe")
                    if i < 20:
                        client.putheader("traceparent", obs.PARENT)
                        client.putheader("x-request-id", f"client-{i}")
                    client.endheaders()
                    assert client.getresponse().status == 200
                    client.close()
                process.wait(timeout=10)
                assert process.returncode == 0, process.stderr.read()
                outgoing = [h["traceparent"] for h in dependency.received]
            spans = []
            for _ in range(40):
                spans = parse_spans(subprocess.run(["docker", "logs", name], capture_output=True, text=True).stderr)
                if len(spans) >= 23 * 3:
                    break
                time.sleep(0.5)
            assert len(spans) == 23 * 3, f"collector decoded {len(spans)} spans, expected {23 * 3}"
            by_id = {s["id"]: s for s in spans}
            assert len(by_id) == len(spans), "duplicate span ids"
            for header in outgoing:
                call = by_id[header[36:52]]
                task = by_id[call["parent"]]
                server = by_id[task["parent"]]
                assert (call["name"], task["name"], server["name"]) == ("http.client", "task", "http.server"), (call, task, server)
                assert (call["kind"], server["kind"]) == ("Client", "Server"), (call, server)
                assert call["trace"] == task["trace"] == server["trace"]
            remote = [s for s in spans if s["name"] == "http.server" and s["parent"] == obs.PARENT[36:52]]
            assert len(remote) == 20 and all(s["trace"] == obs.TRACE for s in remote), "remote parent not preserved"
            roots = [s for s in spans if s["name"] == "http.server" and not s["parent"]]
            assert len(roots) == 3 and all(s["trace"] != obs.TRACE for s in roots), "requests without parent must start new traces"
            print("observability: 69 spans decoded by the official OpenTelemetry Collector, 23 parent/child chains verified")
        finally:
            subprocess.run(["docker", "rm", "-f", name], capture_output=True)


if __name__ == "__main__":
    main()

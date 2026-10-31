#!/usr/bin/env python3
"""Issue #548: automatic message-boundary spans for @Amqp10Consumer and @Amqp091Consumer against a
REAL RabbitMQ 4 and the OFFICIAL OpenTelemetry Collector. One program runs both consumers; each
handler makes an outbound HTTP call. Verified from what the Collector decoded: one consumer span per
message and kind, every message in its own trace (no bleed on the reused consumer thread), each with
its outbound call as child. Skips without Docker.
"""
import base64
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("col", ROOT / "scripts/check-observability-collector.py")
col = importlib.util.module_from_spec(spec)
spec.loader.exec_module(col)
obs = col.obs
COUNT = 6

MAIN = """import C10
import C091

class Main
{
    fnc main() -> Int32
    {
        return 0
    }
}
"""
C10 = """import tinox.core.amqp10
import tinox.core.http

@Amqp10Consumer("127.0.0.1", __PORT__, "tinox", "tinox", "/queues/otel10")
class C10
{
    @OnMessage
    fn onMessage(msg: Amqp10Message) -> Nothing
    {
        const response = Http::get("http://127.0.0.1:__DEP__/amqp10")
    }
}
"""
C091 = """import tinox.core.amqp091
import tinox.core.http

@Amqp091Consumer("127.0.0.1", __PORT__, "/", "tinox", "tinox", "otel091")
class C091
{
    @OnMessage
    fn onMessage(msg: AmqpMessage091) -> Nothing
    {
        const response = Http::get("http://127.0.0.1:__DEP__/amqp091")
    }
}
"""
TOML = """[package]
compiler = "2.8.0"
name = "amqpobs"
version = "0.1.0"
description = ""

[[dependencies]]
group = "tinox.core"
artifactId = "amqp10"
version = "%s"

[[dependencies]]
group = "tinox.core"
artifactId = "amqp091"
version = "%s"

[[dependencies]]
group = "tinox.core"
artifactId = "http"
version = "1.1.0"
"""


def version(module):
    for line in (ROOT / f"crates/tinox-core-ext/{module}/tinox.toml").read_text().splitlines():
        if line.startswith("version = "):
            return line.split('"')[1]


def docker_port(name, port):
    out = subprocess.run(["docker", "port", name, port], capture_output=True, text=True).stdout.strip()
    return int(out.splitlines()[0].rsplit(":", 1)[1]) if out else None


def api(mgmt, method, path, body=None):
    request = urllib.request.Request(f"http://127.0.0.1:{mgmt}/api/{path}", method=method,
                                     data=json.dumps(body).encode() if body is not None else None,
                                     headers={"Content-Type": "application/json",
                                              "Authorization": "Basic " + base64.b64encode(b"tinox:tinox").decode()})
    with urllib.request.urlopen(request, timeout=10) as r:
        return r.read().decode()


def main():
    if shutil.which("docker") is None:
        print("SKIP: docker not installed")
        return
    pid = os.getpid()
    collector, rabbit = f"tinox-otel-amqp-{pid}", f"tinox-otel-rabbit-{pid}"
    process = None
    with tempfile.TemporaryDirectory(prefix="tinox-otel-amqp-") as work:
        config = Path(work) / "collector.yaml"
        config.write_text(col.CONFIG)
        os.chmod(work, 0o755)
        os.chmod(config, 0o644)
        app = Path(work) / "app"
        app.mkdir()
        try:
            subprocess.run(["docker", "run", "-d", "--name", collector, "-p", "127.0.0.1:0:4318",
                            "-v", f"{config}:/etc/otelcol/config.yaml:ro", col.IMAGE, "--config", "/etc/otelcol/config.yaml"],
                           check=True, capture_output=True)
            subprocess.run(["docker", "run", "-d", "--rm", "--name", rabbit, "-e", "RABBITMQ_DEFAULT_USER=tinox",
                            "-e", "RABBITMQ_DEFAULT_PASS=tinox", "-p", "127.0.0.1:0:5672", "-p", "127.0.0.1:0:15672",
                            "rabbitmq:4-management"], check=True, capture_output=True)
            otlp = amqp = mgmt = None
            for _ in range(240):
                otlp, amqp, mgmt = docker_port(collector, "4318/tcp"), docker_port(rabbit, "5672/tcp"), docker_port(rabbit, "15672/tcp")
                if otlp and amqp and mgmt:
                    try:
                        api(mgmt, "GET", "overview")
                        if "Everything is ready" in subprocess.run(["docker", "logs", collector], capture_output=True, text=True).stderr:
                            break
                    except OSError:
                        pass
                time.sleep(0.5)
            else:
                raise AssertionError("collector or broker did not become ready")
            for queue in ("otel10", "otel091"):
                api(mgmt, "PUT", f"queues/%2F/{queue}", {"durable": True, "auto_delete": False, "arguments": {}})
            with obs.peer() as dependency:
                (app / "tinox.toml").write_text(TOML % (version("amqp10"), version("amqp091")))
                (app / "Main.tnx").write_text(MAIN)
                (app / "C10.tnx").write_text(C10.replace("__PORT__", str(amqp)).replace("__DEP__", str(dependency.server_port)))
                (app / "C091.tnx").write_text(C091.replace("__PORT__", str(amqp)).replace("__DEP__", str(dependency.server_port)))
                tinox = ROOT / "target/release/tinox"
                override = f'\n[[overrides]]\ngroup = "tinox.core"\nartifactId = "%s"\npath = "{ROOT}/crates/tinox-core-ext/%s"\n'
                with open(app / "tinox.toml", "a") as f:
                    f.write(override % ("amqp10", "amqp10") + override % ("amqp091", "amqp091"))
                subprocess.run([str(tinox), "install"], cwd=app, check=True, capture_output=True)
                build = subprocess.run([str(tinox), "build", "Main.tnx", "-o", "amqpobs"], cwd=app, capture_output=True, text=True)
                assert build.returncode == 0, build.stdout + build.stderr
                for queue in ("otel10", "otel091"):
                    for i in range(COUNT):
                        r = api(mgmt, "POST", "exchanges/%2F/amq.default/publish",
                                {"properties": {"content_type": "text/plain"}, "routing_key": queue, "payload": f"m{i}", "payload_encoding": "string"})
                        assert '"routed":true' in r, r
                env = {k: v for k, v in os.environ.items() if not k.startswith("TINOX_TELEMETRY_")}
                env.update({"TINOX_TELEMETRY_ENABLED": "true", "TINOX_TELEMETRY_ENDPOINT": f"http://127.0.0.1:{otlp}/v1/traces"})
                process = subprocess.Popen([str(app / "amqpobs")], cwd=app, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                deadline = time.time() + 60
                while len(dependency.received) < COUNT * 2 and time.time() < deadline:
                    time.sleep(0.2)
                assert len(dependency.received) == COUNT * 2, f"handlers ran {len(dependency.received)} times, expected {COUNT * 2}"
                spans = []
                for _ in range(40):
                    spans = [s for s in col.parse_spans(subprocess.run(["docker", "logs", collector], capture_output=True, text=True).stderr)
                             if s["name"] != "task"]
                    if len(spans) >= COUNT * 4:
                        break
                    time.sleep(0.5)
                assert len(spans) == COUNT * 4, f"collector decoded {len(spans)} message-graph spans, expected {COUNT * 4}"
                by_id = {s["id"]: s for s in spans}
                for kind in ("amqp10.consume", "amqp091.consume"):
                    mine = [s for s in spans if s["name"] == kind]
                    assert len(mine) == COUNT and all(s["kind"] == "Consumer" and not s["parent"] for s in mine), (kind, mine)
                assert len({s["trace"] for s in spans if s["name"].endswith(".consume")}) == COUNT * 2, "context bled between messages"
                for headers in dependency.received:
                    call = by_id[headers["traceparent"][36:52]]
                    parent = by_id[call["parent"]]
                    assert call["name"] == "http.client" and parent["name"].endswith(".consume") and call["trace"] == parent["trace"]
                print(f"observability: {COUNT} AMQP 1.0 + {COUNT} AMQP 0-9-1 messages -> {COUNT * 2} separate consumer traces with their outbound calls as children (RabbitMQ 4, official Collector)")
        finally:
            if process and process.poll() is None:
                process.kill()
                process.wait()
            subprocess.run(["docker", "rm", "-f", collector, rabbit], capture_output=True)


if __name__ == "__main__":
    main()

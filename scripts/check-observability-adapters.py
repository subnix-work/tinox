#!/usr/bin/env python3
"""Issue #548: automatic message-boundary spans (WebSocket @OnMessage) against the OFFICIAL
OpenTelemetry Collector. Compiles a real @WebsocketEndpoint program whose handler makes an outbound
HTTP call, sends MESSAGES text frames over ONE reused connection with an independent raw-socket
client, and checks from what the Collector decoded: one `ws.message` consumer span per message, each
in its OWN trace (no context bleed on the reused worker), with the handler's http.client span as its
child and the dependency receiving that child's traceparent. Skips without Docker.
"""
import base64
import importlib.util
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("col", ROOT / "scripts/check-observability-collector.py")
col = importlib.util.module_from_spec(spec)
spec.loader.exec_module(col)
obs = col.obs
MESSAGES = 10

MAIN = """import EchoEndpoint

class Main
{
    fnc main() -> Int32
    {
        return 0
    }
}
"""
ENDPOINT = """import tinox.core.websocket
import tinox.core.http

@WebsocketEndpoint("/echo", __WSPORT__)
class EchoEndpoint
{
    @OnMessage
    fn onMessage(conn: Int64, msg: String) -> Nothing
    {
        const response = Http::get("http://127.0.0.1:__DEPPORT__/dep")
        Ws::sendText(conn, "echo: " + msg)
    }
}
"""
TOML = """[package]
compiler = "2.8.0"
name = "wsobs"
version = "0.1.0"
description = ""

[[dependencies]]
group = "tinox.core"
artifactId = "websocket"
version = "1.1.0"

[[dependencies]]
group = "tinox.core"
artifactId = "http"
version = "1.1.0"
"""


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def frame(text):
    payload = text.encode()
    mask = os.urandom(4)
    masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    return bytes([0x81, 0x80 | len(payload)]) + mask + masked


def read_frame(sock):
    head = sock.recv(2)
    length = head[1] & 0x7F
    data = b""
    while len(data) < length:
        data += sock.recv(length - len(data))
    return data.decode()


def main():
    if shutil.which("docker") is None:
        print("SKIP: docker not installed")
        return
    name = f"tinox-otel-ws-{os.getpid()}"
    with tempfile.TemporaryDirectory(prefix="tinox-otel-ws-") as work:
        config = Path(work) / "collector.yaml"
        config.write_text(col.CONFIG)
        os.chmod(work, 0o755)
        os.chmod(config, 0o644)
        app = Path(work) / "app"
        app.mkdir()
        ws_port = free_port()
        with obs.peer() as dependency:
            (app / "tinox.toml").write_text(TOML)
            (app / "Main.tnx").write_text(MAIN)
            (app / "EchoEndpoint.tnx").write_text(ENDPOINT.replace("__WSPORT__", str(ws_port)).replace("__DEPPORT__", str(dependency.server_port)))
            tinox = ROOT / "target/release/tinox"
            subprocess.run([str(tinox), "install"], cwd=app, check=True, capture_output=True)
            subprocess.run([str(tinox), "build", "Main.tnx", "-o", "wsobs"], cwd=app, check=True, capture_output=True)
            subprocess.run(["docker", "run", "-d", "--name", name, "-p", "127.0.0.1:0:4318",
                            "-v", f"{config}:/etc/otelcol/config.yaml:ro", col.IMAGE, "--config", "/etc/otelcol/config.yaml"],
                           check=True, capture_output=True)
            process = None
            try:
                port = None
                for _ in range(60):
                    out = subprocess.run(["docker", "port", name, "4318/tcp"], capture_output=True, text=True).stdout.strip()
                    if out and "Everything is ready" in subprocess.run(["docker", "logs", name], capture_output=True, text=True).stderr:
                        port = int(out.splitlines()[0].rsplit(":", 1)[1])
                        break
                    time.sleep(0.5)
                assert port, "collector did not become ready"
                env = {k: v for k, v in os.environ.items() if not k.startswith("TINOX_TELEMETRY_")}
                env.update({"TINOX_TELEMETRY_ENABLED": "true", "TINOX_TELEMETRY_ENDPOINT": f"http://127.0.0.1:{port}/v1/traces"})
                process = subprocess.Popen([str(app / "wsobs")], cwd=app, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                sock = None
                for _ in range(100):
                    try:
                        sock = socket.create_connection(("127.0.0.1", ws_port), timeout=5)
                        break
                    except OSError:
                        time.sleep(0.1)
                assert sock, "websocket endpoint did not start"
                key = base64.b64encode(os.urandom(16)).decode()
                sock.sendall((f"GET /echo HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                              f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
                reply = b""
                while b"\r\n\r\n" not in reply:
                    reply += sock.recv(1024)
                assert b" 101 " in reply.split(b"\r\n")[0], reply
                for i in range(MESSAGES):
                    sock.sendall(frame(f"m{i}"))
                    assert read_frame(sock) == f"echo: m{i}"
                sock.close()
                spans = []
                for _ in range(40):
                    spans = col.parse_spans(subprocess.run(["docker", "logs", name], capture_output=True, text=True).stderr)
                    if len([s for s in spans if s["name"] != "task"]) >= MESSAGES * 2:
                        break
                    time.sleep(0.5)
                # The connection worker's own `task` span (child of the still-running listener task)
                # is exported when the connection closes; it is not part of the message graph.
                spans = [s for s in spans if s["name"] != "task"]
                assert len(spans) == MESSAGES * 2, f"collector decoded {len(spans)} message-graph spans, expected {MESSAGES * 2}"
                by_id = {s["id"]: s for s in spans}
                messages = [s for s in spans if s["name"] == "ws.message"]
                assert len(messages) == MESSAGES and all(s["kind"] == "Consumer" and not s["parent"] for s in messages), messages
                assert len({s["trace"] for s in messages}) == MESSAGES, "context bled between messages on the reused connection"
                assert len(dependency.received) == MESSAGES
                for headers in dependency.received:
                    call = by_id[headers["traceparent"][36:52]]
                    parent = by_id[call["parent"]]
                    assert call["name"] == "http.client" and parent["name"] == "ws.message" and call["trace"] == parent["trace"]
                print(f"observability: {MESSAGES} WebSocket messages -> {MESSAGES} separate consumer traces, each with its outbound call as child (official Collector)")
            finally:
                if process and process.poll() is None:
                    process.kill()
                    process.wait()
                subprocess.run(["docker", "rm", "-f", name], capture_output=True)


if __name__ == "__main__":
    main()

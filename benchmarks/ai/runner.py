#!/usr/bin/env python3
"""Reproducible, provider-neutral AI task evaluations. See README.md."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
TOOLS = {
    "read": "{path, offset?, limit?}: read text from a workspace file or docs/<configured-context-name>; offsets in characters, limit <= 12000",
    "list": "{}: list workspace files",
    "write": "{path, content}: write a workspace file",
    "build": "{}: compile Main.tnx",
    "run": "{}: execute the last build (10 second timeout)",
    "submit": "{}: independently check a snapshot; only pass/fail is returned",
}
SYSTEM = ("Complete the Tinox task. Reply with one JSON object containing tool and args. "
          "Use only these tools: " + json.dumps(TOOLS) +
          ". Acceptance checks are held out. Submit when ready. Do not request human help.")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def tree_hash(path):
    return digest(b"".join(str(p.relative_to(path)).encode() + b"\0" + p.read_bytes()
                           for p in sorted(path.rglob("*")) if p.is_file()
                           and "__pycache__" not in p.parts))


def process(command, cwd, timeout, payload=None, env=None):
    """Kill the entire process group on timeout, including spawned service workers."""
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        p = subprocess.Popen(command, cwd=cwd, stdin=subprocess.PIPE, stdout=out,
                             stderr=err, text=True, env=env, start_new_session=True)
        try:
            p.communicate(payload, timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)
            p.communicate()
            raise TimeoutError("process deadline exceeded") from None
        finally:
            # Also clean up descendants after an ordinary parent exit.
            try:
                os.killpg(p.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        out.seek(0)
        err.seek(0)
        return p.returncode, out.read(2_000_000).decode(errors="replace"), err.read(12000).decode(errors="replace")


def local_path(root, name):
    path = (root / name).resolve()
    if not path.is_relative_to(root.resolve()):
        raise ValueError("path outside workspace")
    return path


def validate(config):
    for key in ("provider", "model", "model_revision", "adapter_version", "condition"):
        if not isinstance(config.get(key), str) or not config[key]:
            raise ValueError(f"missing {key}")
    if not isinstance(config.get("command"), list) or not config["command"] or not all(isinstance(x, str) for x in config["command"]):
        raise ValueError("command must be a nonempty argv array")
    for key in ("runs", "max_turns", "context_tokens", "max_output_tokens", "token_budget"):
        if type(config.get(key)) is not int or config[key] <= 0:
            raise ValueError(f"{key} must be a positive integer")
    if config["runs"] < 2:
        raise ValueError("at least two runs are required to measure variability")
    for key in ("max_cost_usd", "input_usd_per_million", "output_usd_per_million", "wall_seconds", "call_seconds"):
        if type(config.get(key)) not in (int, float) or not math.isfinite(config[key]) or config[key] < 0:
            raise ValueError(f"invalid {key}")
    if config.get("billing") not in ("local", "metered"):
        raise ValueError("billing must explicitly be local or metered")
    if config["billing"] == "metered" and (config["max_cost_usd"] <= 0 or
            config["input_usd_per_million"] + config["output_usd_per_million"] <= 0):
        raise ValueError("metered providers require explicit rates and a positive cost cap")
    if not isinstance(config.get("context", {}), dict):
        raise ValueError("context must map documentation names to paths")
    if config["wall_seconds"] == 0 or config["call_seconds"] == 0:
        raise ValueError("timeouts must be positive")
    if not isinstance(config.get("settings"), dict):
        raise ValueError("settings must explicitly specify model sampling options")
    # Configuration and transcripts are public artifacts. Credentials belong only in adapter env.
    for key in config:
        if key not in {"provider", "model", "model_revision", "adapter_version", "condition", "command",
                       "runs", "max_turns", "context_tokens", "max_output_tokens", "token_budget",
                       "max_cost_usd", "input_usd_per_million", "output_usd_per_million", "wall_seconds",
                       "call_seconds", "settings", "context", "billing"}:
            raise ValueError(f"unknown configuration field: {key}")


def evaluate(config, task, tinox, output, run_index, remaining_cost):
    spec = json.loads((HERE / "tasks" / task / "task.json").read_text())
    start = time.monotonic()
    deadline = start + config["wall_seconds"]
    record = {"task": task, "run": run_index, "status": "turn_limit", "correct": False,
              "input_tokens": 0, "output_tokens": 0, "cost_usd": 0.0, "accounting_complete": True,
              "tool_calls": 0, "build_attempts": 0, "build_failures": 0,
              "acceptance_attempts": 0, "first_attempt_success": False, "manual_interventions": 0,
              "events": []}
    messages = [{"role": "system", "content": SYSTEM}, {"role": "user", "content": spec["prompt"]}]
    with tempfile.TemporaryDirectory(prefix="tinox-ai-") as directory:
        workspace = Path(directory) / "work"
        shutil.copytree(HERE / "tasks" / task / "starter", workspace)
        context = {name: Path(path).read_text() for name, path in config.get("context", {}).items()}
        messages[1]["content"] += "\nAvailable documentation: " + json.dumps(sorted(context))
        for turn in range(config["max_turns"]):
            # Reserve the worst-case request BEFORE invoking a possibly paid provider.
            reserve_in = config["context_tokens"]
            reserve_out = config["max_output_tokens"]
            reserve_cost = (reserve_in * config["input_usd_per_million"] +
                            reserve_out * config["output_usd_per_million"]) / 1_000_000
            if record["input_tokens"] + record["output_tokens"] + reserve_in + reserve_out > config["token_budget"]:
                record["status"] = "token_limit"
                break
            if record["cost_usd"] + reserve_cost > remaining_cost:
                record["status"] = "cost_limit"
                break
            if time.monotonic() >= deadline:
                record["status"] = "wall_limit"
                break
            # Conservative UTF-8 byte bound plus per-message template allowance. No history eviction.
            prompt_bound = sum(len(m["content"].encode()) + 256 for m in messages)
            if prompt_bound + reserve_out > reserve_in:
                record["status"] = "context_limit"
                break
            request = {"protocol": 1, "model": config["model"], "model_revision": config["model_revision"],
                       "settings": config["settings"], "context_tokens": reserve_in,
                       "max_output_tokens": reserve_out, "messages": messages}
            event = {"turn": turn + 1, "request": json.loads(json.dumps(request))}
            record["events"].append(event)
            try:
                code, raw, _ = process(config["command"], workspace,
                                       min(config["call_seconds"], deadline - time.monotonic()),
                                       json.dumps(request))
                if code:
                    raise ValueError("adapter failed")
                response = json.loads(raw)
                usage = response["usage"]
                for key in ("input_tokens", "output_tokens"):
                    if type(usage[key]) is not int or usage[key] < 0:
                        raise ValueError("missing or invalid provider usage")
                event["usage"] = usage
                for key in ("input_tokens", "output_tokens"):
                    record[key] += usage[key]
                cost = (usage["input_tokens"] * config["input_usd_per_million"] +
                        usage["output_tokens"] * config["output_usd_per_million"]) / 1_000_000
                record["cost_usd"] += cost
                if usage["input_tokens"] > reserve_in or usage["output_tokens"] > reserve_out:
                    raise ValueError("adapter violated reserved token limits")
                content = response["content"]
                if not isinstance(content, str):
                    raise ValueError("adapter content must be text")
                event["response"] = content
            except (ValueError, KeyError, TypeError, TimeoutError, OSError):
                record["status"] = "provider_error"
                record["accounting_complete"] = False
                # A failed request might still be billed; charge its entire reservation.
                if "usage" not in event:
                    record["cost_usd"] += reserve_cost
                break
            messages.append({"role": "assistant", "content": content})
            try:
                action = json.loads(content)
                tool, args = action["tool"], action.get("args", {})
                event["tool"] = tool
                record["tool_calls"] += 1
                timeout = min(120, deadline - time.monotonic())
                if timeout <= 0:
                    raise TimeoutError("wall limit")
                if tool == "list":
                    result = "\n".join(str(p.relative_to(workspace)) for p in sorted(workspace.rglob("*")) if p.is_file())
                elif tool == "read":
                    name = args["path"]
                    text = context[name[5:]] if name.startswith("docs/") else local_path(workspace, name).read_text()
                    offset, limit = args.get("offset", 0), args.get("limit", 12000)
                    if type(offset) is not int or type(limit) is not int or offset < 0 or not 1 <= limit <= 12000:
                        raise ValueError("invalid read range")
                    result = text[offset:offset + limit]
                elif tool == "write":
                    path = local_path(workspace, args["path"])
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(args["content"])
                    result = "written"
                elif tool == "build":
                    record["build_attempts"] += 1
                    try:
                        code, stdout, stderr = process([tinox, "build", "Main.tnx", "-o", "program"], workspace, timeout)
                    except (OSError, TimeoutError):
                        record["build_failures"] += 1
                        raise
                    record["build_failures"] += int(code != 0)
                    event["failed"] = code != 0
                    result = json.dumps({"exit": code, "stdout": stdout, "stderr": stderr})
                elif tool == "run":
                    code, stdout, stderr = process([str(workspace / "program")], workspace, min(10, timeout))
                    result = json.dumps({"exit": code, "stdout": stdout, "stderr": stderr})
                elif tool == "submit":
                    record["acceptance_attempts"] += 1
                    with tempfile.TemporaryDirectory(prefix="tinox-heldout-") as snapshot:
                        shutil.copytree(workspace, Path(snapshot) / "work")
                        code, _, _ = process([sys.executable, str(HERE / "checks" / "accept.py"), task,
                                              str(Path(snapshot) / "work"), tinox], HERE, timeout)
                    record["correct"] = code == 0
                    result = "acceptance passed" if code == 0 else "acceptance failed"
                    if code == 0:
                        record["status"] = "passed"
                        record["first_attempt_success"] = record["acceptance_attempts"] == 1
                else:
                    raise ValueError("unknown tool")
            except (ValueError, KeyError, TypeError, OSError, TimeoutError) as error:
                result = f"tool error: {type(error).__name__}"
                event["failed"] = True
            event["result"] = result[:12000]
            messages.append({"role": "user", "content": event["result"]})
            if record["correct"]:
                break
        record["wall_seconds"] = time.monotonic() - start
        # Preserve submitted source for review; never preserve provider environment or adapter stderr.
        source = {str(p.relative_to(workspace)): p.read_text() for p in workspace.rglob("*")
                  if p.is_file() and (p.suffix == ".tnx" or p.name in ("tinox.toml", "tinox.lock"))}
        record["source"] = source
    (output / f"{task}-{run_index:03}.json").write_text(json.dumps(record, indent=2) + "\n")
    return record


def summary(records):
    result = {}
    for task in sorted({r["task"] for r in records}):
        runs = [r for r in records if r["task"] == task]
        stats = {"runs": len(runs), "successes": sum(r["correct"] for r in runs),
                 "first_attempt_successes": sum(r["first_attempt_success"] for r in runs),
                 "incomplete_accounting": sum(not r["accounting_complete"] for r in runs)}
        for key in ("input_tokens", "output_tokens", "wall_seconds", "cost_usd", "build_failures", "tool_calls"):
            values = [r[key] for r in runs]
            stats[key] = {"mean": statistics.mean(values), "stdev": statistics.stdev(values) if len(values) > 1 else 0,
                          "min": min(values), "max": max(values)}
        result[task] = stats
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--tinox", type=Path, default=REPO / "target/release/tinox")
    parser.add_argument("--tasks", nargs="+", choices=["bugfix", "validation", "crud"], default=["bugfix", "validation", "crud"])
    parser.add_argument("--execute", action="store_true", help="explicitly authorize provider calls")
    args = parser.parse_args()
    config = json.loads(args.config.read_text())
    validate(config)
    if not args.execute:
        print("Configuration valid. No provider calls made. Add --execute to run.")
        return 0
    args.out.mkdir(parents=True, exist_ok=False)
    tinox = str(args.tinox.resolve())
    code, version, _ = process([tinox, "--version"], REPO, 15)
    if code:
        raise ValueError("compiler version probe failed")
    _, commit, _ = process(["git", "rev-parse", "HEAD"], REPO, 15)
    _, dirty, _ = process(["git", "status", "--porcelain"], REPO, 15)
    metadata = {"protocol": 1, "config": config, "tasks": args.tasks,
                "system_instructions": SYSTEM,
                "adapter_files_sha256": {str(Path(arg).resolve()): digest(Path(arg).read_bytes())
                                         for arg in config["command"] if Path(arg).is_file()},
                "compiler_version": version.strip(),
                "compiler_sha256": digest(Path(tinox).read_bytes()), "repository_commit": commit.strip(),
                "dirty_tree": bool(dirty), "suite_sha256": tree_hash(HERE / "tasks"),
                "checker_sha256": tree_hash(HERE / "checks"), "runner_sha256": digest(Path(__file__).read_bytes()),
                "context_sha256": {k: digest(Path(v).read_bytes()) for k, v in config.get("context", {}).items()},
                "package_manifests": {task: {str(p.relative_to(HERE / "tasks" / task)): p.read_text()
                                      for p in (HERE / "tasks" / task).rglob("tinox.toml")} for task in args.tasks},
                "build_environment": {key: os.environ.get(key) for key in ("CC", "TINOX_CFLAGS", "TINOX_PORT")},
                "platform": platform.platform(), "python": platform.python_version(),
                "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    (args.out / "protocol.json").write_text(json.dumps(metadata, indent=2) + "\n")
    records = []
    remaining = config["max_cost_usd"]
    for index in range(1, config["runs"] + 1):
        for task in args.tasks:
            record = evaluate(config, task, tinox, args.out, index, remaining)
            records.append(record)
            remaining -= record["cost_usd"]
            (args.out / "summary.json").write_text(json.dumps(summary(records), indent=2) + "\n")
            if not record["accounting_complete"]:
                return 1  # Do not make more calls when billing is unknown.
    return int(any(not r["correct"] for r in records))


if __name__ == "__main__":
    sys.exit(main())

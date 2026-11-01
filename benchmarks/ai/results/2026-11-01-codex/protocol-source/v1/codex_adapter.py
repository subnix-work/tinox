#!/usr/bin/env python3
"""One fresh Codex CLI turn, using ChatGPT subscription authentication only.

No native tools or conversation resume: the runner owns tools and full history.
The CLI does not expose immutable backend revisions or a hard output-token cap;
these limitations must accompany results. Usage includes the CLI's own overhead.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

INSTRUCTIONS = ("You are a single-response Tinox evaluation model. The user provides a JSON "
                "conversation transcript. Continue that conversation by returning exactly one "
                "JSON tool action as instructed by its system message. Do not use native tools, "
                "make plans, or emit commentary or Markdown. The external runner executes the action.")
DISABLED = ("shell_tool", "apps", "plugins", "remote_plugin", "hooks", "multi_agent",
            "memories", "shell_snapshot", "browser_use", "computer_use", "image_generation",
            "view_image", "skill_search", "unbounded_connection_retries", "sleep_tool", "goals",
            "code_mode_host", "code_mode", "code_mode_only")


def environment():
    # Do not inherit API keys, alternate endpoints, or parent-session instructions.
    allowed = {"PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TZ", "TERM",
               "CODEX_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME",
               "SSL_CERT_FILE", "SSL_CERT_DIR", "HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY"}
    return {key: value for key, value in os.environ.items() if key in allowed}


def parse_events(raw):
    events = [json.loads(line) for line in raw.splitlines() if line.strip()]
    completions = [e for e in events if e.get("type") == "turn.completed"]
    messages = []
    for event in events:
        kind = event.get("type")
        if kind in ("error", "turn.failed"):
            raise ValueError("Codex turn failed; preserve the attempt and stop")
        if kind in ("item.started", "item.updated", "item.completed"):
            item = event["item"]
            if item["type"] == "error" and item.get("message", "").startswith(
                    "Code Mode is unavailable because code-mode host is disabled."):
                continue  # Expected fail-closed notice for intentionally disabled native tools.
            if item["type"] not in ("agent_message", "reasoning"):
                raise ValueError("native tool call, retry, or unexpected item invalidates this turn")
            if kind == "item.completed" and item["type"] == "agent_message":
                messages.append(item["text"])
    if len(completions) != 1 or len(messages) != 1:
        raise ValueError("expected one complete, tool-free model response")
    usage = completions[0]["usage"]
    for key in ("input_tokens", "output_tokens", "cached_input_tokens"):
        if type(usage.get(key)) is not int or usage[key] < 0:
            raise ValueError("missing CLI-reported usage")
    # output_tokens includes generated reasoning; do not add reasoning_output_tokens twice.
    return {"content": messages[0], "usage": usage,
            "provider_metadata": {"events": events, "backend_snapshot": "not exposed by CLI",
                                  "billing": "ChatGPT subscription; additional spend disabled"}}


def main():
    call = json.load(sys.stdin)
    settings = call["settings"]
    if set(settings) != {"cli_version", "reasoning_effort"}:
        raise ValueError("explicit CLI version and reasoning effort required")
    expected = f"{call['model']}@{settings['cli_version']};backend-snapshot-unavailable"
    if call["model_revision"] != expected:
        raise ValueError("revision must honestly record the unexposed backend snapshot")
    env = environment()
    version = subprocess.run(["codex", "--version"], env=env, capture_output=True, text=True, check=True).stdout.strip()
    if version != settings["cli_version"]:
        raise ValueError("Codex CLI version changed")
    login = subprocess.run(["codex", "login", "status"], env=env, capture_output=True, text=True, check=True)
    if "Logged in using ChatGPT" not in login.stdout + login.stderr:
        raise ValueError("ChatGPT subscription login required; API fallback forbidden")
    with tempfile.TemporaryDirectory(prefix="tinox-codex-turn-") as directory:
        root = Path(directory)
        instructions = root / "instructions.txt"
        instructions.write_text(INSTRUCTIONS)
        config = {"forced_login_method": "chatgpt", "model_provider": "openai",
                  "model_reasoning_effort": settings["reasoning_effort"],
                  "model_instructions_file": str(instructions), "project_doc_max_bytes": 0,
                  "web_search": "disabled", "mcp_servers": {}, "service_tier": "default",
                  "suppress_unstable_features_warning": True,
                  "features.skip_host_skill_discovery": True}
        config.update({"features." + feature: False for feature in DISABLED})
        command = ["codex", "exec", "--ignore-user-config", "--ignore-rules", "--skip-git-repo-check",
                   "--ephemeral", "--json", "-s", "read-only", "-m", call["model"]]
        for key, value in config.items():
            command.extend(["-c", key + "=" + json.dumps(value)])
        command.append("-")
        response = subprocess.run(command, cwd=root, env=env, input=json.dumps(call["messages"]),
                                  text=True, capture_output=True)
        # Failure events are returned to the runner as evidence without storing stderr/credentials.
        try:
            result = parse_events(response.stdout)
            if response.returncode:
                raise ValueError("CLI exit failure")
        except (ValueError, KeyError, TypeError):
            print(json.dumps({"error": "Codex CLI failed or returned an invalid turn",
                              "provider_metadata": {"exit_code": response.returncode,
                                                    "events": response.stdout.splitlines()}}))
            return 0
        print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""One provider call per invocation through the Claude Code CLI (`claude -p`).

Uses the developer's own Claude Code login: no API key and no metered billing here, but every
call consumes subscription quota. Claude Code is run as a plain text model: all of its tools are
disabled, its own system prompt is replaced, no settings/MCP/CLAUDE.md are loaded (it runs in an
empty directory) and no session is stored. The eval runner supplies the tool loop.

Not controllable through the CLI: sampling settings (temperature/seed) are ignored, so repeat
runs differ; use several repetitions. Extended thinking is disabled (MAX_THINKING_TOKENS=0) so
output stays within the runner's output limit and costs less quota. A reply that is exactly one
markdown code fence around the JSON action is unwrapped (models add fences although told not to;
that is formatting, not language ability). Anything else is passed through unchanged, so real
protocol violations are still counted as failures. The pinned `model` must be an exact model id; the call
fails if the CLI reports a different model in `modelUsage` (no silent alias drift).
"""
import json
import os
import subprocess
import sys
import tempfile

TIMEOUT_SECONDS = 300
# Appended to the runner's system prompt: Claude is trained for tool-calling harnesses and otherwise
# answers in prose or pseudo-XML. This is part of the adapter version (v3+), not of the task.
FORMAT_RULES = (
    "\n\nOUTPUT FORMAT (strict): your entire reply is exactly ONE raw JSON object with the keys "
    "\"tool\" and \"args\", for example {\"tool\":\"read\",\"args\":{\"path\":\"Main.tnx\"}}. "
    "No prose before or after, no markdown fences, no XML tags, no explanations, one action per reply. "
    "You have no other tools; the harness executes your action and replies with its result."
)


def build_prompt(messages):
    """Return (system_prompt, transcript). Prior turns become a labelled transcript."""
    system = "\n\n".join(m["content"] for m in messages if m["role"] == "system") + FORMAT_RULES
    turns = [m for m in messages if m["role"] != "system"]
    if len(turns) == 1 and turns[0]["role"] == "user":
        return system, turns[0]["content"]
    parts = []
    for m in turns:
        label = "USER" if m["role"] == "user" else "ASSISTANT (you, earlier)"
        parts.append(f"[{label}]\n{m['content']}")
    parts.append("[ASSISTANT (you, now)]\nReply with your next message only, following the system instructions.")
    return system, "\n\n".join(parts)


def unwrap_fence(text):
    """Strip one surrounding ```/```json fence; leave every other text untouched."""
    stripped = text.strip()
    if stripped.startswith("```") and stripped.endswith("```") and stripped.count("```") == 2:
        body = stripped[3:-3]
        first, _, rest = body.partition("\n")
        if first.strip().lower() in ("", "json"):
            return rest.strip()
    return text


def parse_result(raw, model):
    """Validate the CLI's JSON and return the runner's adapter response."""
    data = json.loads(raw)
    if data.get("is_error") or data.get("subtype") != "success":
        raise ValueError(f"claude CLI call failed: {data.get('subtype')} {data.get('result')!r}")
    if data.get("num_turns") != 1:
        raise ValueError("expected exactly one model turn")
    used = list(data.get("modelUsage", {}))
    if used != [model]:
        raise ValueError(f"CLI used {used}, pinned model is {model}")
    usage = data["usage"]
    input_tokens = (usage["input_tokens"] + usage.get("cache_creation_input_tokens", 0)
                    + usage.get("cache_read_input_tokens", 0))
    return {"content": unwrap_fence(data["result"]),
            "usage": {"input_tokens": input_tokens, "output_tokens": usage["output_tokens"]}}


def main():
    call = json.load(sys.stdin)
    system, prompt = build_prompt(call["messages"])
    command = [os.environ.get("TINOX_EVAL_CLAUDE_BIN", "claude"), "-p", "--tools", "",
               "--no-session-persistence", "--setting-sources", "", "--strict-mcp-config",
               "--model", call["model"], "--output-format", "json", "--system-prompt", system]
    with tempfile.TemporaryDirectory(prefix="tinox-eval-claude-") as empty:
        done = subprocess.run(command, input=prompt, capture_output=True, text=True,
                              cwd=empty, timeout=TIMEOUT_SECONDS,
                              env={**os.environ, "MAX_THINKING_TOKENS": "0"})
    if done.returncode != 0:
        raise ValueError(f"claude CLI exited {done.returncode}")
    print(json.dumps(parse_result(done.stdout, call["model"])))


if __name__ == "__main__":
    main()

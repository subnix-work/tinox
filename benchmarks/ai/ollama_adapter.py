#!/usr/bin/env python3
"""One provider call per invocation; no tools, retries, or hidden requests."""
import json
import os
import sys
import urllib.request


def request(base, path, data=None):
    payload = None if data is None else json.dumps(data).encode()
    req = urllib.request.Request(base + path, data=payload, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=300) as response:
        return json.load(response)


def main():
    call = json.load(sys.stdin)
    base = os.environ.get("TINOX_EVAL_OLLAMA_URL", "http://127.0.0.1:11434").rstrip("/")
    models = request(base, "/api/tags")["models"]
    model = next(m for m in models if m["name"] == call["model"])
    if model["digest"] != call["model_revision"]:
        raise ValueError("model digest differs from pinned revision")
    options = dict(call["settings"])
    options.update(num_ctx=call["context_tokens"], num_predict=call["max_output_tokens"])
    response = request(base, "/api/chat", {"model": call["model"], "messages": call["messages"],
                                          "stream": False, "format": "json", "options": options})
    if not response.get("done"):
        raise ValueError("incomplete generation")
    # Includes the full prompt on every turn (also retrieval/repair history) and generated thinking tokens.
    print(json.dumps({"content": response["message"]["content"],
                      "usage": {"input_tokens": response["prompt_eval_count"],
                                "output_tokens": response["eval_count"]}}))


if __name__ == "__main__":
    main()

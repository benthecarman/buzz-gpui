#!/usr/bin/env python3
"""A deterministic ACP agent for tests: no model, no API keys.

Speaks just enough of the Agent Client Protocol (JSON-RPC over stdio) for
buzz-acp. On each prompt it finds the Buzz event block buzz-acp framed
(`Channel: <name> (#<uuid>)` ... `Content: <text>`) and replies in that
channel with `echo: <text>`, the way real agents reply: through the `buzz`
CLI, which reads the agent's key and relay from the environment buzz-acp
passes down. $FAKE_ACP_BUZZ_CLI names the CLI; otherwise `buzz` on PATH.
"""

import json
import os
import re
import subprocess
import sys

CHANNEL = re.compile(r"^Channel: (?:.* \(#)?([0-9a-f-]{36})\)?$", re.MULTILINE)


def write(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def log(text):
    print(f"fake-acp-agent: {text}", file=sys.stderr, flush=True)


def prompt_text(params):
    return "\n".join(
        part.get("text", "")
        for part in (params or {}).get("prompt", [])
        if isinstance(part, dict) and part.get("type") == "text"
    )


def last_content(prompt):
    """The newest event's content: from its last `Content: ` line up to the
    `Tags: ` line that follows it (content may span lines)."""
    lines = prompt.split("\n")
    tags = [i for i, line in enumerate(lines) if line.startswith("Tags: ")]
    for end in reversed(tags):
        for start in range(end - 1, -1, -1):
            if lines[start].startswith("Content: "):
                return "\n".join([lines[start][len("Content: "):]] + lines[start + 1:end])
    return None


def reply(prompt):
    channels = CHANNEL.findall(prompt)
    content = last_content(prompt)
    if not channels or content is None:
        log("prompt has no Buzz event to answer")
        return None
    # The newest event comes last.
    text = f"echo: {content}"
    cli = os.environ.get("FAKE_ACP_BUZZ_CLI", "buzz")
    try:
        result = subprocess.run(
            [cli, "messages", "send", "--channel", channels[-1], "--content", text],
            capture_output=True,
            text=True,
        )
    except OSError as error:
        log(f"could not run {cli}: {error}")
        return None
    if result.returncode != 0:
        log(f"buzz messages send failed ({result.returncode}): {result.stderr.strip()}")
        return None
    return text


def main():
    sessions = 0
    for line in sys.stdin:
        if not line.strip():
            continue
        request = json.loads(line)
        method, rid = request.get("method"), request.get("id")
        if rid is None or not isinstance(method, str):
            continue  # notifications (session/cancel) and responses
        params = request.get("params") or {}
        if method == "initialize":
            version = params.get("protocolVersion", 1)
            write({"jsonrpc": "2.0", "id": rid,
                   "result": {"protocolVersion": version, "agentCapabilities": {}}})
        elif method == "session/new":
            sessions += 1
            write({"jsonrpc": "2.0", "id": rid,
                   "result": {"sessionId": f"fake-session-{sessions}"}})
        elif method == "session/prompt":
            text = reply(prompt_text(params))
            if text:
                write({"jsonrpc": "2.0", "method": "session/update", "params": {
                    "sessionId": params.get("sessionId"),
                    "update": {"sessionUpdate": "agent_message_chunk",
                               "content": {"type": "text", "text": text}}}})
            write({"jsonrpc": "2.0", "id": rid, "result": {"stopReason": "end_turn"}})
        else:
            write({"jsonrpc": "2.0", "id": rid, "error": {
                "code": -32601, "message": f"fake agent does not support {method}"}})


if __name__ == "__main__":
    main()

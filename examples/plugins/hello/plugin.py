#!/usr/bin/env python3
"""A Buzz plugin in one file, with no dependencies.

Buzz starts this program and talks JSON-RPC 2.0 to it, one message per
line on stdin and stdout. Print logs to stderr: stdout is the protocol.
See docs/plugins.md.

It adds:
- a "Hello" page in the channel sidebar with a counter, a note field and
  the community's channels;
- a panel for https://example.com/ links, with a button in the channel
  header.
"""

import json
import sys

views = {}  # view id -> what the app opened it with, plus our state
pending = {}  # our request id -> what to do with the answer
next_request = 0


def send(message):
    message["jsonrpc"] = "2.0"
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def notify(method, params):
    send({"method": method, "params": params})


def request(method, params, then):
    global next_request
    next_request += 1
    pending[next_request] = then
    send({"id": next_request, "method": method, "params": params})


def text(value, style="body", tone="default"):
    return {"type": "text", "text": value, "style": style, "tone": tone}


def page(state):
    channels = state.get("channels")
    if channels is None:
        listing = [{"type": "spinner"}]
    elif not channels:
        listing = [text("No channels yet.", tone="subtle")]
    else:
        listing = [
            {
                "type": "row",
                "children": [
                    {"type": "icon", "name": "Hash" if c["kind"] != "dm" else "User"},
                    text(c["name"]),
                ]
                + ([{"type": "badge", "text": "private"}] if c["private"] else []),
            }
            for c in channels[:20]
        ]
    return {
        "type": "column",
        "gap": "md",
        "children": [
            text("Hello from Python", style="title"),
            text(
                "This page is drawn by a plugin process. Edit plugin.py and save: "
                "Buzz restarts it and this page redraws.",
                tone="subtle",
            ),
            {
                "type": "card",
                "title": "Counter",
                "children": [
                    {
                        "type": "row",
                        "children": [
                            {
                                "type": "button",
                                "id": "add",
                                "label": "Add one",
                                "variant": "prominent",
                                "icon": "Plus",
                            },
                            {"type": "button", "id": "reset", "label": "Reset"},
                            text(f"Clicked {state['count']} times"),
                        ],
                    }
                ],
            },
            {
                "type": "card",
                "title": "Notes",
                "children": [
                    {
                        "type": "input",
                        "id": "note",
                        "placeholder": "Write a note and press Enter",
                        # Buzz keeps what is typed and empties the field
                        # on Enter, so typing never waits on this plugin.
                        "clearOnSubmit": True,
                    },
                ]
                + [
                    {"type": "markdown", "text": f"- {note}"}
                    for note in state["notes"]
                ],
            },
            {
                "type": "card",
                "title": "Channels in this community",
                "children": listing,
            },
            {
                "type": "row",
                "children": [
                    {"type": "button", "id": "toast", "label": "Show a toast"},
                    {"type": "link", "label": "Open example.com", "url": "https://example.com/"},
                ],
            },
        ],
    }


def panel(state):
    return {
        "type": "column",
        "gap": "md",
        "children": [
            text("Example link", style="heading"),
            # From a link, the target is the link; from the channel
            # header, it is the channel id.
            text(
                state["target"]
                if state["target"].startswith("https://")
                else "Opened from the channel header.",
                style="code",
            ),
            {"type": "divider"},
            text(f"Channel {state.get('channel')}", tone="subtle"),
            {"type": "button", "id": "toast", "label": "Say hi"},
        ],
    }


def render(view):
    state = views[view]
    tree = page(state) if state["kind"] == "page" else panel(state)
    notify("view/render", {"view": view, "tree": tree})


def on_open(params):
    view = params["view"]
    views[view] = {**params, "count": 0, "notes": []}
    render(view)
    if params["kind"] == "page":

        def got_channels(result):
            if view in views:
                views[view]["channels"] = result or []
                render(view)

        request("community/channels", {}, got_channels)


def on_event(params):
    view, element, event = params["view"], params["id"], params["event"]
    state = views.get(view)
    if state is None:
        return
    if element == "add" and event == "click":
        state["count"] += 1
    elif element == "reset" and event == "click":
        state["count"] = 0
    elif element == "note" and event == "submit":
        note = params.get("value", "").strip()
        if not note:
            return
        state["notes"].append(note)
    elif element == "toast" and event == "click":
        request("ui/toast", {"title": "Hello from a plugin", "tone": "success"}, lambda _: None)
        return
    else:
        return
    render(view)


def handle(message):
    method = message.get("method")
    if method is None:
        then = pending.pop(message.get("id"), None)
        if then:
            then(message.get("result"))
        return
    params = message.get("params") or {}
    if "id" in message:
        if method == "initialize":
            send(
                {
                    "id": message["id"],
                    "result": {
                        "pages": [{"id": "hello", "title": "Hello", "icon": "Smiley"}],
                        "panels": [
                            {
                                "id": "example",
                                "title": "Example",
                                "icon": "Globe",
                                "matches": ["https://example.com/"],
                                "launcher": {"label": "Example", "placement": "side"},
                            }
                        ],
                    },
                }
            )
        else:
            send({"id": message["id"], "error": {"code": -32601, "message": method}})
    elif method == "view/open":
        on_open(params)
    elif method == "view/event":
        on_event(params)
    elif method == "view/close":
        views.pop(params.get("view"), None)


def main():
    print("hello plugin started", file=sys.stderr)
    while True:
        line = sys.stdin.readline()
        if not line:
            break  # Buzz closed stdin: time to exit.
        if line.strip():
            handle(json.loads(line))


if __name__ == "__main__":
    main()

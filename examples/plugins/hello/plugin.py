#!/usr/bin/env python3
"""A Buzz plugin in one file, with no dependencies.

Buzz starts this program and talks JSON-RPC 2.0 to it, one message per
line on stdin and stdout. Print logs to stderr: stdout is the protocol.
See docs/plugins.md.

It adds:
- a "Hello" page in the channel sidebar: a counter, notes, and the
  community's channels. Choosing a channel follows it live, and you can
  reply from the page (the manifest declares "send-messages" for that);
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


def request(method, params, then=None):
    """Asks Buzz for something; `then(result, error)` gets the answer."""
    global next_request
    next_request += 1
    pending[next_request] = then
    send({"id": next_request, "method": method, "params": params})


def text(value, style="body", tone="default"):
    return {"type": "text", "text": value, "style": style, "tone": tone}


def channel_list(state):
    channels = state.get("channels")
    if channels is None:
        return [{"type": "spinner"}]
    if not channels:
        return [text("No channels yet.", tone="subtle")]
    return [
        {
            "type": "row",
            "children": [
                {
                    "type": "button",
                    "id": f"follow:{c['id']}",
                    "label": c["name"],
                    "icon": "User" if c["kind"] == "dm" else "Hash",
                    "variant": "prominent" if c["id"] == state.get("following") else "ghost",
                }
            ]
            + ([{"type": "badge", "text": "private"}] if c["private"] else []),
        }
        for c in channels[:20]
    ]


def live(state):
    """The followed channel's newest messages, and a reply field."""
    if not state.get("following"):
        return []
    messages = state.get("messages")
    if messages is None:
        body = [{"type": "spinner"}]
    elif not messages:
        body = [text("No messages yet.", tone="subtle")]
    else:
        body = [
            {
                "type": "column",
                "gap": "none",
                "children": [
                    text(m["authorName"] or m["author"][:10], style="label"),
                    {"type": "markdown", "text": m["content"]},
                ]
                + ([text(m["delivery"], style="small", tone="subtle")] if m["delivery"] else []),
            }
            for m in messages
        ]
    return [
        {
            "type": "card",
            "title": "Latest messages",
            "children": body
            + [
                {
                    "type": "input",
                    "id": "reply",
                    "placeholder": "Send a message to this channel",
                    "clearOnSubmit": True,
                },
            ]
            + ([text(state["error"], tone="danger")] if state.get("error") else [])
            + [{"type": "button", "id": "open", "label": "Open in Buzz", "icon": "ArrowSquareOut"}],
        }
    ]


def page(state):
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
                + [{"type": "markdown", "text": f"- {note}"} for note in state["notes"]],
            },
            {
                "type": "card",
                "title": "Channels in this community",
                "children": channel_list(state),
            },
        ]
        + live(state)
        + [
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

        def got_channels(result, error):
            if view in views:
                views[view]["channels"] = result or []
                render(view)

        request("community/channels", {}, got_channels)


def follow(view, channel):
    state = views[view]
    if state.get("watch"):
        request("channel/unwatch", {"watch": state["watch"]})
    state.update(following=channel, watch=None, messages=None, error=None)

    def watching(result, error):
        if view not in views:
            return
        if error:
            views[view]["error"] = error["message"]
        else:
            views[view]["watch"] = result["watch"]
        render(view)

    request("channel/watch", {"channel": channel, "limit": 10}, watching)


def on_changed(params):
    """A followed channel changed: show its newest messages."""
    for view, state in views.items():
        if state.get("watch") == params["watch"]:
            state["messages"] = params["messages"]
            render(view)


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
    elif element.startswith("follow:") and event == "click":
        follow(view, element.split(":", 1)[1])
    elif element == "reply" and event == "submit":
        content = params.get("value", "").strip()
        if not content:
            return

        def sent(result, error):
            if view in views:
                views[view]["error"] = error["message"] if error else None
                render(view)

        request("messages/send", {"channel": state["following"], "content": content}, sent)
        return
    elif element == "open" and event == "click":
        request("ui/openConversation", {"channel": state["following"]})
        return
    elif element == "toast" and event == "click":
        request("ui/toast", {"title": "Hello from a plugin", "tone": "success"})
        return
    else:
        return
    render(view)


def handle(message):
    method = message.get("method")
    if method is None:
        then = pending.pop(message.get("id"), None)
        if then:
            then(message.get("result"), message.get("error"))
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
    elif method == "channel/changed":
        on_changed(params)


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

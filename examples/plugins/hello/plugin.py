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
  header;
- a Settings section, a notification category, two message actions (one
  marks messages), a chip for example.com links, a card for messages that
  start with "!hello", a composer button, "/" commands in the composer and
  a shortcut (Ctrl/Cmd+Shift+H);
- an agent tool every agent can call, and a "Greeter" kind of agent that
  answers mentions.
"""

import json
import sys
import time

views = {}  # view id -> what the app opened it with, plus our state
pending = {}  # our request id -> what to do with the answer
next_request = 0
starred = set()  # message ids the "Star" action marks
# What typing "/" in the composer offers.
COMMANDS = {"shrug": "¯\\_(ツ)_/¯", "tableflip": "(╯°□°)╯︵ ┻━┻", "wave": "👋"}
settings = {"loud": False}  # what the Settings section edits


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
                    {"type": "button", "id": "notify", "label": "Notify me", "icon": "Bell"},
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


def settings_section(state):
    return {
        "type": "column",
        "gap": "md",
        "children": [
            text("Hello settings", style="title"),
            text("This section is drawn by the plugin too.", tone="subtle"),
            {
                "type": "switch",
                "id": "loud",
                "checked": settings["loud"],
                "label": "Shout in toasts",
            },
        ],
    }


def render(view):
    state = views[view]
    kind = state["kind"]
    tree = page(state) if kind == "page" else settings_section(state) if kind == "settings" else panel(state)
    notify("view/render", {"view": view, "tree": tree})


def toast(title, tone="success"):
    if settings["loud"]:
        title = title.upper() + "!"
    request("ui/toast", {"title": title, "tone": tone})


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
        notify("page/badge", {"page": "hello", "text": str(state["count"])})
    elif element == "reset" and event == "click":
        state["count"] = 0
        notify("page/badge", {"page": "hello", "text": None})
    elif element == "loud" and event == "toggle":
        settings["loud"] = params.get("value", False)
    elif element == "notify" and event == "click":
        global next_request
        request(
            "notifications/submit",
            {
                "category": "greetings",
                "sourceKey": f"hello-{next_request}",
                "title": "Hello!",
                "body": "A notification from a plugin.",
            },
        )
        return
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
        toast("Hello from a plugin")
        return
    else:
        return
    render(view)


def link_chip(url):
    """A link renderer's answer: what the chip for an example.com link shows."""
    path = url[len("https://example.com"):] or "/"
    return {
        "type": "row",
        "gap": "sm",
        "children": [{"type": "icon", "name": "Globe"}, text(f"Example · {path}")],
    }


def hello_card(message):
    """A message renderer's answer: a card for "!hello" messages."""
    rest = message["content"][len("!hello"):].strip() or "Hello!"
    return {
        "type": "card",
        "title": f"{message['authorName'] or 'Someone'} says hello",
        "children": [
            {"type": "markdown", "text": rest},
            {"type": "button", "id": "wave", "label": "Wave back", "icon": "Smiley"},
        ],
    }


def on_action(params):
    action, message = params["action"], params["message"]
    if action == "words":
        count = len(message["content"].split())
        toast(f"{count} words")
    elif action == "star":
        starred.symmetric_difference_update({message["id"]})
        notify("message/markers", {"action": "star", "messages": sorted(starred)})


def run_agent(run_request, params):
    """A Greeter run: answer a mention in its thread, as the agent."""
    trigger = params["trigger"]
    if trigger["kind"] != "mention":
        send({"id": run_request, "result": None})
        return
    event = trigger["event"]
    name = params["agent"]["name"]

    def sent(result, error):
        # Answering agent/run ends the run.
        if error:
            send({"id": run_request, "error": {"code": 1, "message": error["message"]}})
        else:
            send({"id": run_request, "result": None})

    text = f"Hello from {name}! You said: {event['content']}"
    request("agent/tool", {"run": params["run"], "tool": "send", "arguments": {"text": text, "audience": "everyone"}}, sent)


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
                                "menu": {"label": "Open the example panel"},
                            }
                        ],
                        "settingsCards": [
                            {"id": "settings", "title": "Hello", "icon": "Smiley", "placement": "Integrations"}
                        ],
                        "notifications": [{"id": "greetings", "label": "Hello greetings"}],
                        "messageActions": [
                            {"id": "words", "title": "Count words", "icon": "TextAa"},
                            {"id": "star", "title": "Star or unstar", "icon": "Flag"},
                        ],
                        "linkRenderers": [{"id": "example-link", "matches": ["https://example.com/"]}],
                        "messageRenderers": [{"id": "hello-card", "prefix": "!hello"}],
                        "composerTools": [{"id": "wave", "title": "Insert a wave", "icon": "Smiley"}],
                        "shortcuts": [{"id": "hi", "title": "Say hi", "keys": ["mod+shift+h"]}],
                        "completions": [{"id": "commands", "title": "Commands", "trigger": "/"}],
                        "agentTools": [
                            {
                                "name": "hello_time",
                                "description": "The time on the computer running Buzz.",
                                "inputSchema": {"type": "object", "properties": {}},
                            }
                        ],
                        "agentTypes": [
                            {
                                "id": "greeter",
                                "title": "Greeter",
                                "description": "Answers every mention with a greeting. Runs in the Hello plugin.",
                                "instructions": "Answer every mention.",
                            }
                        ],
                    },
                }
            )
        elif method == "link/render":
            send({"id": message["id"], "result": {"tree": link_chip(params["url"])}})
        elif method == "agentTool/call" and params["tool"] == "hello_time":
            now = time.strftime("%Y-%m-%d %H:%M:%S %Z")
            send({"id": message["id"], "result": {"text": f"It is {now}."}})
        elif method == "agent/run":
            run_agent(message["id"], params)
        elif method == "completion/query":
            query = params["query"].lower()
            items = [
                {"label": f"/{name}", "detail": text, "text": text + " "}
                for name, text in COMMANDS.items()
                if name.startswith(query)
            ]
            send({"id": message["id"], "result": {"items": items}})
        elif method == "message/render":
            send({"id": message["id"], "result": {"tree": hello_card(params["message"])}})
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
    elif method == "message/action":
        on_action(params)
    elif method == "composer/tool":
        request("composer/insert", {"composer": params["composer"], "text": "👋 "})
    elif method == "shortcut":
        toast("Hi from a shortcut")
    elif method == "render/event" and params.get("id") == "wave":
        toast("You waved back")


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

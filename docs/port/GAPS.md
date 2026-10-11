# Parity gaps with buzz-app

What buzz-app does that this port does not yet do, or does differently.
The goal is one-to-one parity. Six audits went through buzz-app area by area
on 2026-10-09 and checked each user-visible behaviour against the port. Each
row cites both codebases and carries a size: S is under a day, M is a few
days, and L is a week or more.

| Area | File | Missing | Partial | Different | Open rows |
|---|---|---:|---:|---:|---:|
| App shell, navigation, windows, updates, notifications | [gaps/shell.md](gaps/shell.md) | 47 | 12 | 12 | 71 |
| Settings, profile, identity | [gaps/settings.md](gaps/settings.md) | 50 | 14 | 28 | 92 |
| Messages, composer, media, search | [gaps/messages.md](gaps/messages.md) | 90 | 42 | 28 | 160 |
| Channels, communities, relay layer | [gaps/channels.md](gaps/channels.md) | 0 | 1 | 11 | 12 |
| Agents, sessions, workflows, terminal | [gaps/agents.md](gaps/agents.md) | 2 | 0 | 7 | 9 |
| Bundled plugins and the rest | [gaps/plugins.md](gaps/plugins.md) | 30 | 6 | 2 | 38 |

Open rows by size: about 805 small, 355 medium, and 75 large. Some
behaviours show up in more than one audit.

The rows marked IN PROGRESS (agent restart on save, provider tests and
Databricks fields, notification sounds, the private key widget, the
status calendar, the default profile, saved agent activity, leaving a
community) were done after the audits ran, as was live Markdown styling
in the composer, with its markers visible rather than hidden.

## buzz-app main since the last pin

On 2026-10-11 the buzz-app fork was rebased onto buzz-app main at #833
(`d55d70a0`), and the port pins the result (fork rev `ec7d795f`). Each
upstream change since the previous base is listed below, with the area file
that holds its rows.

| Upstream change | Area | Status |
|---|---|---|
| #609 `08b3cbe8` share compact clock formatting | [messages](gaps/messages.md) | DONE |
| #818 `8820e5a5` bundle the Claude Code agent type by default | [plugins](gaps/plugins.md), [agents](gaps/agents.md) §30 | DONE |
| #788 `635b699c` placeholder Builderlab agent type in Agents2 | [agents](gaps/agents.md) §30 | SKIPPED (decided: Builderlab) |
| #751 `89ee72a9` native Codex agents with CLI defaults | [agents](gaps/agents.md) §30 | DONE |
| #769 `bd7797f7` bounded stdin for fixed plugin host commands | [agents](gaps/agents.md) §30 | N/A (external plugins only) |
| #813 `5608988d` Sonner for bottom-right app toasts | [shell](gaps/shell.md) §11 | DONE |
| #775 `8cef21d0` shared team instructions in their own record | [agents](gaps/agents.md) §30 | DONE |
| #824 `54bd0634` complete the Builderlab mock in a test | — | N/A (test only) |
| `b23fb875` Codex agents in Agents2 | [agents](gaps/agents.md) §30 | DONE |
| #790 `5fe10620` run each Agents2 agent in one copy of the app | [agents](gaps/agents.md) §30 | DONE |
| #819 `0217167f` inbox detail heading opens its context | [plugins](gaps/plugins.md) | DONE |
| #797 `2b079c66` test-audit agent skill | — | N/A (contributor docs) |
| #823 `63d2f5ee` rebuild Vite's dependency cache on dev start | — | N/A (web dev tooling) |
| #830 `fcb3c655` bundle the Codex agent type by default | [plugins](gaps/plugins.md), [agents](gaps/agents.md) §30 | DONE |
| #829 `829b1ecc` split an invitation paging test | — | N/A (test only) |
| #832 `f927c881` run bundled Codex with danger-full-access | [agents](gaps/agents.md) §30 | DONE |
| #826 `293c5191` view Codex and Claude sessions started in Buzz | [agents](gaps/agents.md) §30 | DONE (runtime naming); scripts N/A |
| #831 `f8705a95` default Codex workspace to `~/.buzz` | [agents](gaps/agents.md) §30 | DONE |
| #803 `f138a467` show typed angle-bracket text | [messages](gaps/messages.md) | DONE |
| #817 `82d165d5` Me sidebar working indicators and activity | [plugins](gaps/plugins.md) | DONE |
| #843 `35972bde` cancel toast exit-removal timers on unmount | [shell](gaps/shell.md) §11 | N/A (Sonner library fix) |
| #825 `a2b7db9a` nonmember mention prompt defaults to Invite | [messages](gaps/messages.md) | DONE |
| #798 `37dcd836` design-system chrome tabs for Me and Messages | [shell](gaps/shell.md) §11 | DONE |
| #851 `6460c4ee` Codex replies in the thread, not under the mention | [agents](gaps/agents.md) §30 | DONE |
| #833 `d55d70a0` commit attribution as Agents2 does it | [agents](gaps/agents.md) §30 | DONE |

## Decided

- **Skipped:**
  - External plugin import, signing and host grants. Bundled plugins are
    ported natively instead.
  - Block-hosted services: Hosted communities, Builderlab and the enterprise
    login gate.
- **Updates** come from GitHub releases. Copies installed from the .deb
  update themselves; macOS, Windows and development builds only say a
  release is out (`crates/app/src/updates/`).
- **Reuse buzz-app's Rust where it exists:**
  - The chat data layer is TypeScript in buzz-app, so `crates/relay` stays
    our Rust port of it.
  - Native services move from the fork's `src-tauri` into a library one at a
    time, as their features are built: media conversion, the upload spool,
    the PTY backend, the model catalogs and OAuth, and the plugin records.

## The large pieces

- **Bundled plugins:** Me, Inbox, Reminders, Projects, GitHub, Workflows,
  Terminal, Templates and teams, Hosted communities, Builderlab, Pair mobile,
  Channel usage and Community admin. buzz-app turns all of these on by
  default except Todos, Templates and teams, and Bestie. Each has pages,
  panels, Settings cards and relay records.
- **Plugin system:** a registry for pages, panels, message, inline and link
  renderers, and Settings cards. Also external plugin import with signing and
  host grants. This port compiles plugins in, so the registry becomes
  internal extension points.
- **Workspace shell:**
  - primary pages as sidebar rows;
  - tabbed channel panels with secondary conversation and thread tabs, and a
    New tab picker;
  - a companion dock;
  - a resizable sidebar;
  - a narrow-window drawer.
- **Relay reliability:** a disk cache of channel heads and profiles, and an
  offline resume that shows saved conversations.
- **Media:** HEIC to JPEG and video to MP4 conversion before upload. Uploads
  are deferred until Send.
- **Composer:** WYSIWYG formatting with no visible Markdown markers, and
  saved teams in mentions.
- **Agents:**
  - the full inventory, import, clone and identity actions;
  - the agent catalog;
  - team snapshots and import;
  - Agents2 and its agent types;
  - sessions, with history, sharing and a runtime;
  - observer telemetry;
  - pairing and remote agent setup;
  - the Pi installer;
  - Databricks browser sign-in.
- **Native extras:** a terminal (PTY) panel per channel, and the in-app
  browser.

## Not applicable

Each audit ends with what doesn't apply to a native Linux app:
- macOS and Windows window behaviour;
- webview-only and dev-server-only behaviour;
- behaviour of buzz-app's JavaScript plugin runtime itself.

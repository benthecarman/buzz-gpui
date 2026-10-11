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

## Decided

- **Skipped:**
  - External plugin import, signing and host grants. Bundled plugins are
    ported natively instead.
  - Block-hosted services: Hosted communities, Builderlab and the enterprise
    login gate.
- **Later:** updates, from GitHub releases eventually.
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
- **Updates:** checking for, downloading and installing updates, and an
  Updates section in Settings.
- **Native extras:** a terminal (PTY) panel per channel, and the in-app
  browser.

## Not applicable

Each audit ends with what doesn't apply to a native Linux app:
- macOS and Windows window behaviour;
- webview-only and dev-server-only behaviour;
- behaviour of buzz-app's JavaScript plugin runtime itself.

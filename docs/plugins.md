# Writing a plugin

A Buzz plugin is a program on your computer. Buzz starts it, asks what it
adds, and draws what it sends with Buzz's own components. You can write it in
any language that reads stdin and writes stdout: a short Python or Node script
is enough, and no SDK is needed.

A plugin can add:

- **Pages**, in the channel sidebar or the header, with a badge.
- **Panels** beside the conversation. A panel opens from a link it matches,
  a button in the channel header or the channel's menu.
- **Settings sections** and **notification categories**.
- **Message actions** in a message's More menu, with an optional marker.
- **Renderers** that draw link chips, message bodies or attachments.
- **Composer tools**, **completions** and **keyboard shortcuts**.
- **Agent tools** every agent session gets, and **kinds of agent** the plugin
  runs.

[`examples/plugins/hello`](../examples/plugins/hello) is a complete plugin in
one Python file with no dependencies. It adds one of each, except an
attachment renderer.

## Try the example

1. Open **Settings → Plugins** and click **Add plugin folder…**.
2. Choose `examples/plugins/hello`. New plugins start off.
3. Read the command it runs, then turn it on. **Hello** appears in the channel
   sidebar.
4. Edit `plugin.py` and save. Buzz restarts the plugin, and the open page
   redraws.

## Trust

A plugin runs with your full access. It is not sandboxed: it can read your
files and use your network like any other program you run. Turn on only
plugins you trust. Settings shows the command a plugin runs before you turn it
on.

This is buzz-app's model too: its plugins are trusted code in the app's own
WebView. Buzz never gives a plugin your private key, and it removes `BUZZ_*`
and `NOSTR_*` variables from the plugin's environment.

Because the key stays in Buzz, acting as you is something Buzz does for the
plugin, and only when the plugin declares it (see `permissions` below).
Settings lists what a plugin can do; turning the plugin on allows it. If a
new version asks for more, Buzz turns the plugin off until you review it and
turn it on again.

## The folder

A plugin folder holds `buzz-plugin.json` and whatever the plugin needs:

```json
{
  "id": "com.example.hello",
  "name": "Hello",
  "description": "A sample page and link panel.",
  "command": ["python3", "plugin.py"],
  "permissions": ["send-messages"]
}
```

| Field | Meaning |
| --- | --- |
| `id` | Unique. Lowercase letters, digits, `.`, `-` and `_`, such as `com.example.weather`. Ids starting with `buzz.` are reserved. |
| `name` | Shown in Settings. 1 to 64 characters. |
| `description` | Optional. Shown in Settings. |
| `command` | The program and its arguments. A program containing `/` is relative to the folder, such as `./plugin`. Otherwise Buzz finds it on `PATH`. |
| `apiVersion` | Optional. `1`, the only version so far. |
| `permissions` | Optional. What the plugin may do as you. `send-messages`: send, edit, react to and delete messages, and upload files to attach. `publish-events`: publish other Nostr events (see `events/publish`). `manage-agents`: create, change and remove your agents and teams. `manage-channels`: start DMs; create, join, leave, archive and delete channels; change members, details, workflows and your kit. Reading needs no permission. |

The plugin runs in its folder. Buzz sets `BUZZ_PLUGIN_ID`,
`BUZZ_PLUGIN_API_VERSION` and `PYTHONUNBUFFERED=1`.

Buzz watches the folder. Saving any file restarts the plugin, except for files
under hidden folders (such as `.cache`), `node_modules`, `target`,
`__pycache__` and `venv`. Keep the plugin's own state under a hidden folder so
that writing it does not restart the plugin.

## How Buzz talks to a plugin

Messages are [JSON-RPC 2.0](https://www.jsonrpc.org/specification), one JSON
object per line, on the plugin's stdin and stdout. stdout is only for
messages: write logs to stderr. Buzz logs each stderr line and shows the last
ones if the plugin stops.

The plugin should exit when stdin closes. To stop a plugin, Buzz closes its
stdin, waits up to two seconds, then ends the plugin and every process it
started.

### 1. `initialize` (request from Buzz)

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"apiVersion":1,"app":{"name":"Buzz","version":"0.1.0"},"host":{"browser":false}}}
```

`host` says what this app offers. `browser` is `false`: there is no embedded
browser, so a `link` element or `ui/openUrl` opens the system's browser.

Answer with what the plugin adds. The answer is due within 10 seconds.

```json
{"jsonrpc":"2.0","id":1,"result":{
  "pages":[{"id":"home","title":"Weather","icon":"Sun"}],
  "panels":[{
    "id":"forecast",
    "title":"Forecast",
    "icon":"Globe",
    "matches":["https://weather.example/"],
    "launcher":{"label":"Forecast","placement":"side"}
  }]
}}
```

- A contribution `id` starts with a lowercase letter or digit, then uses
  those, `.`, `-` and `_`. Titles are
  1 to 64 characters.
- `icon` names a Buzz icon, such as `House`, `Globe`, `Bell`, `Gear`, `Code`
  or `Plug`. An unknown name shows the plug icon. A page's `icon` can also be
  a `data:image/` URL (PNG, JPEG, GIF, WebP or SVG, up to 64 KiB), drawn at
  16px in the sidebar and search, as buzz-app's page icons are.
- `matches` lists link prefixes. A matching link clicked in a message opens
  the panel as a tab. When several panels match, the lowest `order` wins,
  then the lowest `plugin/id` key. Buzz's own panels are at 0. By convention,
  use -10 to specialise links another panel shows, 0 (the default) for your
  own links and 100 for a catch-all.
- `launcher` adds a button to the channel header. `placement` is `side` (the
  default) or `bottom`; `target` is what the panel opens showing (the
  channel's id by default). `appLauncher` (`{"label","target"}`) adds a
  button to the app's header instead, which opens the panel at `target`
  beside any page and keeps it there while you move between pages.
  Outside a conversation, `ui/openPanel`, `panel/open` and a matching
  link open the panel there too. `menu` (`{"label"}`) adds an entry to the channel's
  menu; `kinds` (such as `["stream", "forum"]`) and `private` (`true` or
  `false`) limit the channels it is offered in.
- A page's `placement` is `sidebar` (the default), `topbar` (`toolbar` is the
  same here) or `hidden`. `layout` is `document` (the default, padded) or
  `workspace` (edge to edge). Buzz's dock shows a page's companion beside
  it. With `routes: true`, `ui/navigate` can open the page at `params` (any
  JSON value but `null`, up to 8 KiB): its view's `view/open` carries `route`, and Buzz
  sends `page/route` with the `view` and `route` when it changes. Back and
  Forward step through routes. `routeVersion` (default 1) says what the
  params mean: raise it when they change, and a `ui/navigate` that names
  another `version` is refused. A page settles a route itself: it draws
  what the route shows, fails it with `page/settle`, or redirects with
  `ui/navigate` and `replace: true`.
- A plugin can add at most 16 panels and 8 pages, and at most 16 of each kind
  below. Ids are unique across all of them.

The rest of the result can list:

| Key | Each entry | What it adds |
| --- | --- | --- |
| `settingsCards` | `id`, `title`, `icon`, `placement`: `community`, `administration` or a group name (default `Integrations`) | A Settings section. It is a view with `kind` `settings`; its `view/open` carries the selected `community`'s relay. |
| `notifications` | `id`, `label` | A category with its own switch in Settings → Notifications. Send with `notifications/submit`. |
| `messageActions` | `id`, `title`, `icon`, `kinds` (default chat messages), `mine` | An item in a message's More menu. Choosing it sends `message/action`. |
| `linkRenderers` | `id`, `matches` (link prefixes), `order` | Draws the chip of matching links in messages. Buzz asks with `link/render`. |
| `messageRenderers` | `id`, `kinds`, `prefix`, `order` | Draws the body of matching messages. Buzz asks with `message/render`. It needs a `prefix` of two or more characters, or kinds other than chat messages. |
| `attachmentRenderers` | `id`, `mime` (MIME type prefixes) | Draws matching attachments, as buzz-app does: audio hosted by the community in messages, in place of the audio player; a voice note being recorded into the composer; and any draft in its preview. A draft not uploaded yet has a local `url` (`draft-audio:`, `voice-draft:` or `draft-file:`) that `media/get` reads. Buzz asks with `attachment/render`. Images, videos and files in messages keep Buzz's own views, and voice notes keep the Voice Notes plugin's card while it is on. |
| `inlineRenderers` | `id`, `pattern` (a regular expression, up to 500 characters, that cannot match empty text) | Draws the pieces of message text the pattern matches, such as ticket numbers. Buzz asks with `inline/render`. Mentions, emoji, links and code are left alone. A reaction whose whole text the pattern matches is drawn the same way. |
| `composerTools` | `id`, `title`, `icon`, `order`, `reactionPicker` | A button in every composer's toolbar. Clicking it sends `composer/tool`. Buzz disables the button while the composer cannot be used, such as while it sends or in a read-only channel. With `reactionPicker: true`, it can also replace the emoji picker that "Add reaction" opens: the tool with the lowest `order`, then the lowest `plugin/id` key, wins, and Buzz's own picker is `buzz.emoji/emoji` at 0. The picker is a view of `kind` `reactionPicker`. Send `reaction/select` with its `view` and an `emoji` to react. |
| `composerAccessories` | `id`, `title`, `order`, `placement`, `kinds` | With `placement` `composer` (the default): a view above every composer, of `kind` `accessory`; `view/open` also carries the composer's `channel` and `thread`. Keep it to a line or two. With `placement` `message`: something beside each message's reactions, such as work on it; Buzz asks `message/accessory`, for chat messages or the `kinds` listed. |
| `namingPolicies` | `id` (at most one) | Display names for people and agents (`identityNames.register`). Buzz asks `names/resolve` and shows public names until the answer comes. As in buzz-app, a policy applies only while it is the only one on: switch off Identity Naming in Settings → Plugins to use a plugin's. |
| `services` | `id` | Something the plugin offers other plugins (`ctx.provide`). Another plugin calls it with `services/request`, and Buzz asks this one `service/request`. The plugin sends the notification `service/publish` (`service`, `event`, `data`) to tell plugins that subscribed. |
| `accountActions` | `id`, `title`, `icon` | A row in the account menu that opens a dialog titled `title`. The dialog's body is a view of `kind` `dialog`. Send `ui/closeView` with its `view` to close it. |
| `completions` | `id`, `title`, `trigger` (one character other than `@`, `#` and `:`), `order` | Typing the trigger at the start of a word in a composer opens the plugin's suggestions. Buzz asks with `completion/query`. When plugins share a trigger, the lowest `order`, then the lowest `plugin/id` key, answers. |
| `shortcuts` | `id`, `title`, `keys` (such as `mod+shift+k`), `order`, `allowInEditable`, `allowInModal`, `repeat` | A shortcut, listed and rebindable in Settings → Shortcuts. Pressing it sends `shortcut`. A chord may be a bare key such as `j` or `?`, and a symbol typed with Shift needs no `shift` (`shift+?` is `?`); Buzz's own chords win. It does not run while a text field has focus or a dialog is open unless `allowInEditable` or `allowInModal` is set, and a held chord runs once unless `repeat` is set. |

### Agents

`agentTools` lists tools every agent gets beside the Buzz tools: Claude,
Codex and the plugin's own agents. Each has a `name` (a lowercase letter,
then lowercase letters, digits or `_`, not a Buzz tool's name), a
`description` and an `inputSchema` (a JSON Schema object). When an agent
calls one, Buzz sends a request:

```json
{"jsonrpc":"2.0","id":4,"method":"agentTool/call","params":{
  "call":12,"tool":"hello_time","arguments":{},
  "agent":"<agent pubkey>","channel":"<channel id>","root":"<thread root id>"
}}
```

Answer with `{"text"}`, `{"image": {"data", "mime"}}` (base64) or an error.
A call may take five minutes. Agent sessions read their tools when they
start, so adding or changing a tool replaces idle Claude sessions (which
resume) and starts new Codex threads.

`agentTypes` lists kinds of agent, which show in Create agent beside Claude
and Codex. Each has an `id`, a `title`, an optional `description`, `config`
(a new agent's settings), `instructions` (a new agent's default Interest),
`attention` (more attention objects a new agent starts with, by slug, as
`agents/save` takes them) and
`timeoutSeconds` (default 300, at most 1800). Each time such an agent wakes,
Buzz sends a request and the run lasts until the plugin answers it:

```json
{"jsonrpc":"2.0","id":5,"method":"agent/run","params":{
  "run":13,"type":"greeter",
  "trigger":{"kind":"mention","event":{"id","pubkey","createdAt","kind","content","tags"}},
  "channel":"<channel id>","root":"<thread root id>",
  "agent":{"pubkey","name","owner"},
  "config":{},"model":null,"effort":null,"team":null
}}
```

A trigger's `kind` is `mention`, `watch` (with `watch`, the slug,
`watchObject`, `interest` and `classifierSkipped`, the reason a classifier did
not run) or `timer` (with `timer`, `timerObject`, `interest` and `spent`). A run
that queues its work can ask `agent/current` with the `run` whether the
trigger should still run. If the run is cancelled, Buzz
sends the notification `agent/cancel` with the `run`.

A type can also describe its agents on the Agents page. Each request's params
are `type` and `agent` (`pubkey`, `name`, `owner`, `config`), and Buzz keeps
the answer until the agent's name or config changes:

- `summary: true`: Buzz asks `agent/summary` for the line under the agent's
  name. Answer `{"text"}` (up to 120 characters) or `null` for the title.
- `peek: true`: Buzz asks `agent/peek` for a glance shown when the agent is
  selected. Answer `{"tree"}` or `null`, as a renderer does.
- `createInstructions: true`: the New agent dialog offers the new agent's
  instructions.
- `icon` names a Buzz icon shown with the type; the robot without one.
- `createForm: true`: the type's own part of the New agent dialog, in place
  of the instructions. It is a view of `kind` `agentCreate` whose `view/open`
  carries `type` and the starting `config`; send `agent/createConfig` with the
  `view` and the whole new `config` as it changes.
- `createModels` (`{"models": [{"id", "label", "description"}], "defaultLabel",
  "allowCustom"}`): model choices in the New agent dialog, saved as the
  config's `model`.
- `models: true`: Buzz asks `agent/models` for the models and effort levels the
  owner can pick while mentioning an agent. Answer `{"current", "models":
  [{"id", "label", "efforts": [{"id", "label"}], "effort"}]}`. The picks reach
  `agent/run` as `model` and `effort`.
- `provider: true`: the type has device-wide readiness under Provider settings
  on the Agents page. Send the notification `agentType/readiness` with `type`,
  `readiness` (`checking`, `ready` or `attention`), and optional
  `attentionLabel`, `blocking` and `action`. Buzz sends `agentType/check` when
  the user asks to check again. The provider's dialog is a view of `kind`
  `agentProvider`; close it with `ui/closeView`. With `action: true`, picking
  the provider while it needs attention sends `agentType/attention` (`type`)
  rather than opening the dialog, for a fix the plugin starts itself, such as
  an install.
- `hosted: true`: the plugin holds each agent's key and runs it, as a hosted
  service would. Buzz never sends `agent/run` for it. Making one asks
  `agent/provision` (`type`, `name`, `community`; answer `{"pubkey"}`, a key
  not yet in use), then `agent/activate` (`type`, `pubkey`, `name`,
  `community`, `auth`: your NIP-OA attestation `["auth", owner, "",
  signature]` to attach to what it signs), then lists it in your agent list
  and, with a picture, asks `agent/profile` (`type`, `pubkey`, `community`,
  `name`, `picture`). Deleting it asks `agent/remove` (`type`, `pubkey`,
  `community`). Each step may come again after a failure, so make each safe to
  repeat. The Agents page shows these agents as Hosted.
- `carry` (`{"instructions", "workspace"}`, each a config key): what another
  type takes over when an agent changes type, and where this type keeps what
  it takes over. `migrationWarning` says what leaving this type costs.
  `migrationSettings` (`[{"id", "label", "key", "default"}]`) lists the
  settings a change of type previews: one is the agent's own when its config
  has `key` set to something other than `default`, and the preview says
  whether the new type keeps it (it has a setting with the same `id` and
  value), resets it or drops it.
- `tabs` (`[{"id", "title", "role"}]`, at most 8; `attention`, `memory` and
  `settings` are Buzz's ids): settings tabs before Attention. Each is a view of `kind` `agentTab`; `view/open` also carries
  `type`, `agent`, `contribution` (the tab id) and `conversation`
  (`{"channel","root"}` when the agent was opened from a message). To save,
  send `agent/save` with the `view` and the whole new `config`. With `role:
  "settings"` the tab is the agent's Settings, first, with Buzz's Provider
  field at its top. When the user picks this type there for an agent of
  another type, the tab opens with `pending: true` and `agent.config` set to
  what this type starts with; `agent/save` then makes the change with that
  config.

While a call or run is open, the plugin can act as its agent. A run that
must keep working after it answers (buzz-app's `workingUntil`) answers, then
opens a call of its own with `agent/open` and acts through that:

| Request | Params | Result |
| --- | --- | --- |
| `agent/open` | `agent` (the pubkey of one of your type's agents on this device), `channel`, `root` (optional) | `{"call"}`, a call that stays open until `agent/close` or the plugin stops |
| `agent/close` | `call` | `null` |
| `agent/tool` | `call` or `run`, `tool` (a Buzz tool, such as `send`, `read`, `react` or `channels`, or in a run one of the attention tools, such as `watch_add`), `arguments` | The tool's answer: `{"text"}` or `{"image"}` |
| `agent/step` | `run`, `tool`, `target` (optional) | `null`. Shows what the run is doing on its message, such as `read` on a channel. |
| `agent/acknowledged`, `agent/superseded`, `agent/ignored` | `run` | `null`. Marks the run's message as taken up, as replaced by a newer request (its bubble hides), or as declined (nothing shows). |
| `agents/activity` | none | What agents on this device are doing, newest first: `[{"agent","channel","message","root","state","startedAt","error","step"}]`, `state` being `waiting`, `working`, `done`, `error` or `superseded` |
| `agent/current` | `run` | Whether the trigger should still run: `false` once the agent is removed, its attention turned off, or its watch changed |
| `agent/query` | `call` or `run`, `filters` (1 to 10 NIP-01 filters) | The events, read as the agent, at most 500 |
| `agent/publish` | `call` or `run`, `kind`, `content`, `tags` | The signed event, published as the agent |
| `agent/upload` | `call` or `run`, `path` (a file, up to the community's limit for its type) or `data` (base64, up to 3 MiB), `mime` | `{"url","sha256","size","type",…}`, uploaded as the agent |

The agent's key stays on its host; Buzz signs for it. Buzz tools that read
local files or memories are not available to plugin agents. `send` needs
`audience`: `everyone`, or `agents` for a message only agents act on.

### Renderers

Buzz draws while it paints and does not wait for a plugin. The first time a
renderer matches something, Buzz asks the plugin, draws what it would have
drawn anyway, and redraws when the answer comes. Answers are kept until the
plugin restarts, so answer from the request alone. Completions work the
same way, per query.

| Request from Buzz | Params | Answer |
| --- | --- | --- |
| `link/render` | `renderer`, `url`, `label` | `{"tree"}`, or `null` to leave the chip as it is |
| `message/render` | `renderer`, `message` | `{"tree"}` or `null` |
| `message/accessory` | `accessory`, `message` | `{"tree"}` to show beside its reactions, or `null`. Asked again when the message is edited or gets a reply. |
| `attachment/render` | `renderer`, `attachment` (`url`, `kind`: `image`, `video`, `audio` or `file`, `mime`, `name`, `size`, `duration`, `waveform`, `voiceNote`, `dimensions` `{"width","height"}`, `blurhash`, `previewUrl`), `removable` (a draft the plugin may take out with `attachment/remove`, buzz-app's `onRemove`) | `{"tree"}` or `null` |
| `inline/render` | `renderer`, `text` (what the pattern matched), `message` (the message it is in), `reaction` (`{"content","emojiUrl","count","authors"}` when the text is a reaction on it, else `null`) | `{"tree"}`, drawn in the line of text, or `null` to leave it as text. Buzz asks once per piece and message, and separately for a reaction. |
| `names/resolve` | `policy`, `identities` (`[{"pubkey","name","isAgent","owner"}]`, at most 5,000), `viewer`, `candidates` (the keys in view, or `null` for everyone) | `{"labels": {"<pubkey>": {"name", "qualifier"}}}`. Keys left out show their public name. |
| `service/request` | `from` (the asking plugin's id), `service`, `method`, `params` | Any JSON, handed back to the plugin that asked. An error is passed on as its error. |
| `completion/query` | `completion`, `request` (this query's id, for `completion/publish`), `query` (the text after the trigger, up to the caret), `channel`, `thread` (the root the composer replies in, or `null`), `inviteAgents` (`true` in a new session's composer, which may offer your library agents not yet in the conversation) | `{"items", "status", "retry"}`: at most 50 items `{"label", "detail", "text", "mentions", "preview", "disabled", "exact", "image", "avatar"}`. `image` (an https or `data:image/` URL) or `avatar` (a pubkey, drawn with the person's picture) shows before the label. Picking one replaces the trigger and query with `text`, or with `mentions` (pubkeys) as chips that make them recipients. `preview` is a line under the label; `disabled` is why it cannot be picked; a space typed after the query picks the item marked `exact`. `status` shows a line such as "No matches", and `retry: true` adds a row that asks again. |

Answers are kept, but not forever:

| Request | Params | Result |
| --- | --- | --- |
| `render/invalidate` | `renderer` (one of yours, or an agent type's id for its summary and peek), and optionally `target` (as `render/event` gives it) or `message` (a message id) | How many answers it marked. Buzz asks again for each the next time it draws, and shows the old answer until the new one comes, as a live buzz-app component updates in place. |
| `completion/publish` | `request` (from `completion/query`), `items`, `status`, `retry` | `true` when the query is still kept; its suggestions are replaced, such as results after a first "Searching…" answer (`publish`). |
| `completion/invalidate` | `completion` | `null`. Every query is asked again the next time it is typed. |

A request that fails or times out draws nothing and is asked again 30
seconds later; an answer of `null` declines until it is invalidated.

A renderer's tree can use every element but `input`. Its buttons and switches
send `render/event` with `renderer`, `target` (what it drew: the URL, the
attachment's URL, or for a message `<message id>:<revision>`), `message`
(the message id, for message renderers and accessories), `id`, `event` and
`value`.

### Other notifications from Buzz

| Notification | Params |
| --- | --- |
| `message/action` | `action`, `message` |
| `composer/tool` | `tool`, `channel`, `thread` (the root it replies in, or `null`), `composer` (an id for `composer/insert`), `inviteAgents` (as in `completion/query`) |
| `shortcut` | `shortcut` |
| `render/event` | `renderer`, `target`, `id`, `event`, `value` |

### Notifications from the plugin

| Notification | Params | Effect |
| --- | --- | --- |
| `message/markers` | `action`, `messages` (event ids) | Those messages show the action's icon after the byline; the list replaces the last one. |
| `page/badge` | `page`, `text` (up to 8 characters, or `null` to clear), `tone` | A badge on the page's row. |
| `shortcut/enable` | `shortcut`, `enabled` | Turns one of the plugin's shortcuts off or back on for now, as buzz-app's `when` does. |
| `settings/visibility` | `card`, `visibility` (`visible`, `hidden`, `pending`, or `error` when the check failed and opening the section should offer a retry) | Lists or hides one of the plugin's Settings sections, such as one only some communities need. Sections start visible. |

### 2. `view/open` (notification from Buzz)

Each open page or panel is a view with its own id. Buzz tells the plugin when
one opens:

```json
{"jsonrpc":"2.0","method":"view/open","params":{
  "view":"v3","contribution":"forecast","kind":"panel",
  "target":"https://weather.example/today","channel":"<channel id>","thread":null,
  "root":"<thread root id>"
}}
```

For a page, `kind` is `page`, and `target` and `channel` are `null`. A panel
opened from the channel header has the channel id as its `target`. `thread`
is the thread showing beside the conversation, and `root` the thread of the
message whose link opened the panel (`context.rootId`), or `null`.

### 3. `view/render` (notification from the plugin)

Send a view's tree whenever it changes. The first one replaces the loading
spinner.

```json
{"jsonrpc":"2.0","method":"view/render","params":{"view":"v3","tree":{
  "type":"column","children":[
    {"type":"text","text":"Sunny","style":"title"},
    {"type":"button","id":"refresh","label":"Refresh"}
  ]
}}}
```

### 4. `view/event` (notification from Buzz)

Clicks, toggles and typing come back as events:

```json
{"jsonrpc":"2.0","method":"view/event","params":{"view":"v3","id":"refresh","event":"click"}}
```

| Element | `event` | `value` |
| --- | --- | --- |
| `button` | `click` | none |
| `switch`, `checkbox` | `toggle` | `true` or `false` |
| `choice` | `change` | the chosen option's `value` |
| `input` | `change` | the field's text |
| `input` | `submit` (Enter) | the field's text |
| `composer` | `sent` | `{"id"}` of the message it sent |
| `composer` | `submit` (with `submit: "plugin"`) | the draft `{"content","mentions","references","attachments"}` |
| `composer` | `ready` | `{"composer"}`, for `composer/insert` |
| `message` | `openThread` (its replies summary) | `{"message","root","channel"}` |
| `message` | `reply` (Reply in its action bar) | `{"message","root","channel"}`: open its thread, or embed a composer for it |
| `message` | `edit` (Edit message) | `{"message","root","channel","content"}` |

### 5. `view/close` (notification from Buzz)

The view is gone. Forget its state.

When the plugin restarts, Buzz sends `initialize` again, then `view/open` for
every view that is still open. Until a new tree arrives, the old one stays on
screen.

## Elements

Every element is an object with a `type`. Unknown types draw nothing, so a
plugin written for a newer Buzz still shows what this one understands. A tree
can hold at most 5,000 elements, nested at most 32 deep.

| `type` | Fields |
| --- | --- |
| `column` | `children`, `gap` (`none`, `sm` (default), `md`, `lg`) |
| `row` | `children`, `gap`, `justify` (`start`, `center`, `end`, `between`), `wrap` |
| `card` | `title` (optional), `children` |
| `text` | `text`, `style` (`body`, `small`, `label`, `title`, `heading`, `code`), `tone` (`default`, `subtle`, `accent`, `success`, `warning`, `danger`) |
| `markdown` | `text`, rendered as a Buzz message is |
| `button` | `id`, `label`, `variant` (`prominent`, `subtle`, `ghost`, `outline`, `destructive`), `icon`, `disabled`, `loading` |
| `link` | `label`, `url`. Opens like a link in a message: a panel that matches it opens; otherwise the browser does. |
| `input` | `id`, `value`, `placeholder`, `version`, `clearOnSubmit` |
| `switch` | `id`, `checked`, `label` |
| `checkbox` | `id`, `checked`, `label`; sends `toggle` like a switch |
| `choice` | `id`, `value`, `options` (`[{"value","label"}]`, up to 50), drawn as radio buttons; sends `change` with the chosen `value` |
| `progress` | `value`, from 0 to 1 |
| `image` | `url` (https, or the community's media), `width` and `height` (pixels, up to 1200), `alt` |
| `composer` | `id`, `channel`, `thread` (optional root), `parent` (optional: the reply it answers in the thread), `placeholder`, `disabled`, `autoFocus`, `draft` (text it starts with, put back whenever it changes), `submit`: Buzz's own composer (only in views, not renderers). Once made it sends `ready` with `{"composer"}`, an id for `composer/insert` and `composer/insertMentions`. It sends as the channel's does, then sends `sent` with the message `{"id"}`. With `submit: "plugin"` it sends nothing: the plugin gets `submit` with the draft `{"content","mentions","references","attachments"}` (attachments already uploaded) and sends it itself, such as once the channel it needs exists. The draft stays, locked, until the plugin calls `composer/settle`. |
| `message` | `id`, `channel`, `message` (an event id): one message as Buzz draws it in a conversation, with its byline, attachments and reactions, kept up to date (only in views). It has the conversation's own action bar: reactions and the picker, copying, read state, follows, delete, report and plugins' message actions all work as in a channel, and a failed send of yours retries from it. Replying, editing and opening its thread go to the plugin, which decides where they happen: see the events below. |
| `thread` | `id`, `channel`, `root`, `height` (pixels, default 480): Buzz's own thread view; closing it sends `close` (only in views) |
| `avatar` | `pubkey`, `size` (`small`, `default` or `large`): their profile picture, or their initial |
| `badge` | `text`, `tone` |
| `icon` | `name` |
| `divider` | none |
| `spinner` | none |

Buzz keeps what is typed in an `input`, so typing never waits on the plugin.
`value` is the text a field starts with. To replace the text later, send a
new `value` with a higher `version`. Set `clearOnSubmit` to empty the field
when Enter sends it.

## Asking Buzz for things

A plugin can send requests to Buzz. Use your own ids; Buzz answers each one.

| Method | Params | Result |
| --- | --- | --- |
| `ui/toast` | `title`, `body` (optional), `tone` (`info`, `success`, `warning`, `error`) | `null` |
| `ui/openUrl` | `url` (http or https), opened in the browser | `null` |
| `community/current` | none | `{"name","relay","viewer","status","error","live","cached","restoring","liveStatus"}` for the selected community, or `null`. `viewer` is your pubkey; `status` is `connecting`, `ready` or `failed` (with `error`); `live` says the live connection is up; `cached` says it shows saved data while the relay has not answered, and `restoring` that it is asking again meanwhile. `liveStatus` is `session.live`: `{"status","error","retryInMs","routes","roster","rosterError","heads","issue"}`, with each route `{"id","channel","status","replay","error"}`, each channel catch-up `{"channel","state","error"}`, and `issue` the first problem worth showing. Buzz sends the notification `community/changed` with the same fields whenever any of them change, such as when another community is selected. |
| `community/refresh` | `what`: `unread`, `inbox`, `reminders`, `emoji` or `kit` | `null`. Reads that view again, as its Retry does after an error. |
| `community/list` | none | Your communities: `[{"name","relay","icon","selected"}]`. Buzz sends `communities/changed` with `communities`, the same list, when you join or leave one or select another. |
| `community/client` | none | `communityReader.snapshot()`: `{"status","viewer","error","profile","communities"}`. `status` is `loading`, `ready` or `unavailable` (no identity yet, or one that could not be read, with `error`); `profile` is the personal profile new communities start from, `{"name","picture","about"}`; `communities` is `community/list`. Works in Personal space. Buzz sends `client/changed` with the same fields when they change. |
| `community/retry` | none | `null`. Connects to the selected community again after a failure. |
| `community/clearCache` | none | `null`, after forgetting the conversations Buzz saved of the community, here and on disk, as Clear cache in Settings does. Open channels load again. |
| `community/channels` | none | The selected community's channels: `[{"id","name","description","private","archived","kind","typeKnown","members","participants","labels","lastActivityAt","updatedAt","preview","readOnly","cached","hidden","creator","createdAt","parent"}]`, where `kind` is `stream`, `forum`, `dm` or `session` |
| `channels/list` | none | `{"status","error","cached","channels","hiddenDms","hiddenStatus","hiddenError"}`: the channels as `community/channels` gives them, with how far the list got (`loading`, `ready` or `error`), whether it is this device's saved copy, and the DMs you hid with how far that read got. |
| `channels/searchPublic` | `query`, `exact` (optional) | `{"channels","partial"}`: open channels you have not joined whose name has `query` (or is it), as read-only previews; `partial` when the relay's page was full. `channel/messages`, `channel/watch` and the thread reads work on them too. |
| `channels/findLabeled` | `creator`, `label` | The channels `creator` made with NIP-CL `label`, oldest first; an error on a relay without labels. |
| `channels/resolve` | `ids` (1 to 50) | The channels you may read among `ids`, joined or not (`channels.get`, `resolve`, `describe`). |
| `channels/members` | `channel` | `{"channel","version","canManage","members"}`: each member `{"pubkey","role","canManage","canRemove"}`, read fresh from the relay-signed roster. `role` is `owner`, `admin`, `member`, `guest`, `bot` or `unknown`, the `expected` that `channels/changeMember` takes; `canRemove` also covers your own agents. |
| `channels/lifecycle` | `channel` | `{"canArchive","canUnarchive","canDelete","canLeave","canHide","leaveReason","deleteUnavailable"}`: what you may do to the channel as a whole (`channelLifecycle.load`). |
| `session/watch` | `what`: `channels`, `channelList`, `sidebar`, `sync`, `presence`, `emoji`, `profiles` (with `pubkeys`, at most 500, read if not loaded), or `typing` (with a `channel`, and a `root` for one thread) or `activity` with a `channel` | `{"watch"}`. Buzz then sends `session/changed` with `what` and `value` (as `community/channels`, `channels/list`, `sidebar/get`, `unread/sync`, `presence/get`, `emoji/list`, `profiles/get`, `typing/get` with `channel` and `anywhere` or a thread's `thread`, or `threads/activity`'s `items`), now and whenever it changes. Stop with `session/unwatch`. |
| `channel/messages` | `channel`, `limit` (default 50, at most 200) | `{"channel","messages","hasMore","stale","error"}`, the newest messages, oldest first. `stale` is true when the relay has not yet confirmed rows saved on this device. For older history, use `events/query` with `until`. |
| `thread/messages` | `channel`, `root` (or `message`, any message in the thread), `exact` (optional: fold the thread around `message` on its own evidence, as a link to one reply does) | `{"channel","root","replies","hasMore","limited","target","targetStatus","error"}`: `limited` when more exists than a view holds; with `exact`, `target` is that message and `targetStatus` `loading`, `ready`, `unavailable` or `error` |
| `channel/watch` | `channel`, `limit` | `{"watch","channel"}`. Buzz then sends `channel/changed` (below). `watch/more` loads older history into it. |
| `thread/watch` | as `thread/messages` | `{"watch","channel","root"}`. Buzz then sends `thread/changed` with what `thread/messages` gives, now and whenever the thread changes. `watch/more` loads more of it. |
| `watch/more` | `watch` (a channel or thread watch) | `true` when it asked for more: older messages for a channel watch, whose `limit` also grows by 50 (to 1,000), or the rest of a thread (`loadMore`, `loadOlder`); `false` for a watch with nothing to load. |
| `events/watch` | `filters` (as `events/query` takes, without the read extensions) | `{"watch"}`. Buzz then sends `events/live` with `events`, the live events that match, from now on. |
| `channel/unwatch`, `thread/unwatch`, `events/unwatch` | `watch` | `null`. A plugin holds at most 64 watches of every kind in all. |
| `events/query` | `filters`: 1 to 10 NIP-01 filters of `ids`, `authors`, `kinds`, `#<letter>` tags, `since`, `until` and `limit`, plus the relay's read extensions: `search` with `search_mode` (`prefix` or `fulltext`), `page`, `feed_types`, `top_level`, `include_aux`, `include_summaries`, `before_id`, `thread_window`, `depth_limit`, `thread_cursor` and `thread_cursor_id` | The matching signed events, at most 500, read from the community as you |
| `community/info` | none | The community relay's NIP-11 document. Its `self` key signs what the relay publishes, such as rosters and the archive. |
| `contributions/list` | none | What Buzz and its plugins added, each by its `plugin/id` `key`: `pages` (`title`, `placement`, `routes`: the route version or `null`), `panels` (`title`, `order`, `launcher`), `settings` (`title`, `visibility`: `visible`, `hidden`, `pending` or `error`), `accountActions` (`title`), `shortcuts` (plugins' only: `plugin`, `title`, `chords` as shown, `enabled`), `composerTools` and `completions` (`order`; completions also `trigger`), `accessories` (`placement`), `inlineRenderers`, `messageRenderers`, `linkRenderers`, `attachmentRenderers` and `messageActions` (with `title`). Buzz sends `contributions/changed` with the same fields when any of them changes. |
| `contributions/update` | everything `initialize` answers (pages, panels, renderers, tools, agent types and the rest) | `null`. Replaces what the plugin contributes without restarting it: what is new is added, what is gone is taken away, and the rest stays, open views included. buzz-app plugins register and dispose contributions at any time; this is how a plugin here adds a page once it learns it needs one. |
| `shortcuts/run` | `key` (a plugin shortcut's) | `true` when it ran, as its chord would (`RegisteredShortcut.run`), such as from a command palette |
| `plugins/list` | none | Every plugin, built in or added: `[{"id","name","builtIn","status","active"}]`, `status` being `running`, `starting`, `off`, `paused` or `failed` (`pluginStatus`). Buzz sends `plugins/changed` with the same list when one changes. |
| `profiles/get` | `pubkeys` (hex, at most 500) | `[{"pubkey","name","picture","about","nip05","isAgent","loaded"}]` |
| `messages/send` | `channel`, `content`, `thread` (optional `{"root","parent"}`), `mentions` (optional pubkeys, notified), `references` (optional pubkeys, shown but not notified), `picks` (optional `{pubkey: {"model","effort"}}` for mentioned agents), `attachments` (optional, from `files/upload`) | `{"id"}`, once the message is queued: follow it with `outbox/get` or `outbox/watch`. `id` is `null` if Buzz changed the text on the way, such as by adding attachment links. Needs `send-messages`. |
| `messages/edit` | `channel`, `message`, `content` | `null` once sent. Only your own messages. Needs `send-messages`. |
| `messages/react` | `channel`, `message`, `emoji` | `null` once sent. Needs `send-messages`. |
| `messages/delete` | `channel`, `message` (a message or reaction) | `null` once sent. Your own, or an agent's you own. Needs `send-messages`. |
| `messages/report` | `channel`, `message`, `reason` (`spam`, `profanity`, `nudity`, `impersonation`, `malware`, `illegal` or `other`), `note` | `null` once the relay accepts it. Needs `send-messages`. |
| `messages/retry`, `messages/dismiss` | `message` (one of yours whose `delivery` is `failed`) | `null`. Sends it again, or drops it. Needs `send-messages`. |
| `files/upload` | `path` (a file, up to the community's limit for its type: 50 MiB for images, 500 MiB for video, 100 MiB otherwise) or `data` (base64, up to 3 MiB); `name` and `mime` (optional; Buzz guesses); `progress` (optional: a name for `files/progress`) | `{"name","url","mime","size","sha256"}`, for `messages/send`'s `attachments`. With `progress`, Buzz sends `files/progress` (`progress`, `stage`: `preparing`, `uploading` or `sending` with `sent` and `total` bytes) as it goes. Needs `send-messages`. |
| `events/publish` | `kind`, `content`, `tags` | `{"id","event"}` once the relay accepts it. Buzz signs as you, only kinds it writes itself (such as 7, 9, 30315 and 40003), plus app data (30078) and the agent and team catalogs (30175, 30178), as buzz-app does. Needs `publish-events`. |
| `ui/navigate` | `to`: `home`, `me`, `agents`, `settings` (with an optional `section`, such as `appearance`, `plugins` or a settings card's `plugin/id`), `page` (with `page`, a page's `plugin/id`, and optional `params` and `version` for a page with `routes`), `conversation` (with `channel`, and optional `message`, `thread` and `members`), `back` or `forward`; optionally `community` (a joined community's `wss://` relay, or `null` to leave for Personal space first); `replace: true` to take the current visit's place in history, such as to redirect a route; and `focus: "preserve"` to leave the focus where it is, for a control that keeps it | Once the visit settles (`OpenResult`): `{"status":"opened"}`, `{"status":"failed","reason"}` with `navigation/get`'s reasons, or `{"status":"superseded"}` when another visit started first. Back and Forward answer `null` at once. A `conversation` may name a channel not in your list, such as an open one you have not joined; it opens if the relay has it. |
| `navigation/get` | none | `{"status","entry","place","failed","canGoBack","canGoForward"}`: `place` is where the window is, in the form `ui/navigate` takes (with `community`), or `null` before anything shows. `status` is `opening` while a conversation is still looked up, `opened`, or `failed` when the visit could not open; `failed` then says which place, why (`OpenFailure`: `denied`, `invalid-target`, `not-found`, `timeout`, `host-error` or `unavailable`), whether it came from outside the window (`ingress`), and whether `navigation/retry` may help. `entry` changes with each visit. |
| `navigation/watch` | none | `{"watch"}`. Buzz then sends `navigation/changed` with what `navigation/get` gives, now and whenever it changes. Stop with `navigation/unwatch`. |
| `navigation/retry` | none | `null`. Opens the visit that failed again. |
| `page/settle` | `view` (an open page), `status`: `failed` or `opened` | `null`. With `failed`, the visit showing the page fails as `unavailable`, kept for Retry navigation, as buzz-app's `PageNavigation.complete({status: "failed"})` does for a route the page cannot show. |
| `format/time` | `at` (unix seconds), `style`: `byline` ("Yesterday at 2:34 PM"), `item` (a list row: "2:34 PM", "Yesterday", "Sat, Jun 20"), `day` (a day divider, "Monday"), `full` (a byline's hover text) or `relative` ("5 minutes ago") | The text, as Buzz words times |
| `ui/openPanel` | `panel` (one of the plugin's panel ids), `target` (what it shows, such as a link or any text the plugin reads from `view/open`) | `null`. Opens the panel as a tab of the open conversation. |
| `panel/canOpen` | `target` | Whether any panel, the plugin's or not, shows `target`. |
| `panel/resolve` | `target` | The panel that would show `target` (`panels.resolve`): `{"key","title","order"}`, `key` being `plugin/panel`, or `null` when none does. |
| `panel/open` | `view` (an open panel), `target` | `true` if a panel shows `target`. When it is this panel, the view shows it in place and the plugin hears `view/retarget` (`view`, `target`, `canGoBack`); otherwise the panel that shows it opens as a tab for the same conversation and thread, and this one closes. |
| `panel/push` | `view` (an open panel), `target` | As `panel/open`, but keeps this panel: within it, Buzz shows a Back button that returns to the previous target with another `view/retarget`; another panel opens beside it. |
| `ui/openLink` | `url` (`buzz://` or a web link) | `null`. Opens it as a click in a message does: in Buzz, in a panel that shows it, or in the browser. |
| `names/get` | `pubkeys` (at most 500), `candidates` (optional: the keys in view, such as a channel's members, that namesakes are told apart among; everyone when left out), `displayFacts` (optional, at most 500 `{"pubkey","name","isAgent","owner"}`: names this view shows, such as a draft's recipients, that the naming policy weighs in place of those keys' profiles) | `{"<pubkey>": {"name","qualifier","source"}}`: names as Buzz shows them, through the naming policy, with the npub suffix that tells namesakes apart. `source` is `agent-directory` when the naming policy named the key, `public-profile` for a profile name, or `null` when the key's start shows. |
| `names/watch` | as `names/get` | `{"watch","names"}`. Buzz then sends `names/changed` with `names` whenever one changes: a profile loads, an agent is renamed, or the naming policy answers or changes. Stop with `names/unwatch`. |
| `ui/openConversation` | `channel`, `message` (optional), `thread` (optional), `members` (optional, `true` to open its members) | `null`. Buzz shows the conversation. |
| `presence/get` | `pubkeys` (at most 500) | `{"<pubkey>": "online" | "away" | "offline" | null}`, for people Buzz already follows; `null` for others |
| `presence/watch` | `pubkeys` (at most 64) | `{"watch"}`. Buzz follows their presence, shown or not, and sends `presence/changed` with `presence` (as `presence/get` gives it) now and whenever it changes. Stop with `presence/unwatch`. |
| `media/get` | `url` (the community's media, or a draft's local `url` from `attachment/render`), and optionally `directory` (with `name`) | `{"data","size","mime"}`: the file as base64, at most 8 MiB, fetched as you; with `directory`, `{"path"}` of the file saved there, any size. Other URLs, fetch yourself. |
| `attachment/remove` | `url` (a draft `attachment/render` called `removable`) | `true` when the draft was removed from its composer |
| `media/url` | `url` (the community's media) | A loopback `http://127.0.0.1` URL to fetch or stream it from, any size, with ranges (`session.media`), or `null` when Buzz cannot serve it so |
| `sidebar/get` | none | `{"status","error","loaded","cached","writable","sortWritable","muteWritable","removeGroupWritable","starred","muted","sort","groups","assignments","sortErrors","moves","hiddenDms","meChannels"}`: your starred and muted channels, each group's sort, your saved groups `{"id","name","icon"}`, which group each channel is in, what can be saved now, sorts that failed `{"group","mode","error"}`, moves still saving or failed `{"channel","to","pending","error"}`, and the DMs and Me conversations the sidebar leaves out |
| `sidebar/retryMove` | `channel` | `null`, after trying a failed move again. Needs `manage-channels`. |
| `sidebar/dismiss` | `channel` (a failed move) or `group` (a failed sort) | `null`. Forgets the failure. |
| `me/get` | none | Your Me groups (`mePreferences`), as `sidebar/get` gives the Messages sidebar: `groups`, `assignments` (conversation → group), `moves`, `status`, `writable` and the rest. |
| `me/assign` | `channel`, and `group` (a group id, or `null` for Conversations) or `newGroup` (a name, to create one) | `null` once saved. Needs `manage-channels`. |
| `me/retryMove`, `me/removeGroup` | `channel`; `group` | `null`. Tries a failed move again; deletes a group, whose conversations go back to Conversations. Need `manage-channels`. |
| `me/dismiss`, `me/refresh` | `channel`; none | `null`. Forgets a failed move; reads the groups again. |
| `sidebar/star`, `sidebar/mute` | `channel`, and `starred` or `muted` (`true` or `false`) | `null` once saved. Unstarring returns a channel to its default section. Needs `manage-channels`. |
| `sidebar/sort` | `group` (`starred`, `channels`, `forums`, `dms` or `section:<id>`), `mode` (`alpha` or `recent`) | `null` once saved. Needs `manage-channels`. |
| `sidebar/assign` | `channel`, and `group` (a saved group id), `newGroup` (a name: makes the group), or neither for its default section | `null` once saved. Needs `manage-channels`. |
| `sidebar/removeGroup` | `group` | `null` once saved; its channels return to their default sections. Needs `manage-channels`. |
| `statuses/get` | `pubkeys` (at most 20) | `{"<pubkey>": {"text","emoji","updatedAt","expiresAt"} | null}`, read fresh |
| `typing/get` | `channel`, `root` (optional, a thread) | The pubkeys typing there |
| `emoji/list` | none | The community's custom emoji: `[{"shortcode","url"}]` |
| `emoji/catalog` | none | `{"status","error","entries","mine"}`: the custom emoji with how far the read got (`idle`, `loading`, `ready`, `error`), and your own set |
| `people/search` | `query`, `page` (from 1, the default) | `{"people","more"}`: profiles that match, as `profiles/get` gives them |
| `notifications/submit` | `category`, `sourceKey` (unique per thing; repeats are dropped), `title`, `body`, and what a click opens: any place `ui/navigate` takes (`to` and its fields), or the short forms `channel` with an optional `message` and `thread`, or a `page` (`plugin/id`) | `true` if it showed. Buzz applies the user's notification settings. |
| `agent/save` | `view` (an open `agentTab`), `config` (an object, the whole config) | `null` once saved |
| `unread/get` | none | `{"channels", "mentions", "direct", "observed"}`. `channels` maps each channel with something unread to `{"unread","count","attention","mentions","threads","manual","manualKind","latestAt","preview","stale","threadsStale"}`, `manualKind` being `none`, `local-only` (this device's mark) or `remote` (synced from another device). Counts are sampled, not exact. |
| `unread/watch` | none | `{"watch"}`. Buzz then sends `unread/changed` with the same fields, now and whenever they change. Stop with `unread/unwatch`. |
| `unread/message` | `channel`, `message` | Whether that loaded message shows as unread now |
| `unread/attention` | `channel`, `message` | That loaded message's attention under the badges' policy (`unread.attention`): `{"status","category","mentioned","root","pending","unread","forced","viewing"}`, `status` being `unknown`, `ineligible` or `eligible` and `category` `mention`, `direct`, `thread` or `null`; `null` before read state loads |
| `unread/sync` | none | `{"capability","status","completeness","error"}` (`capability` `unsupported`, `read-only` or `frontier-sync`; `completeness` `unknown`, `snapshot` or `bounded`, also sent as `complete`): how read marks sync to the relay, or `null` before it is known. `unread/retrySync` tries again after a failure. |
| `threads/activity` | `channel` | `{"channel","items","coverage","freshness","error"}`: threads with unread replies for you, newest first, each `{"root","latestMessage","author","workflowOwner","createdAt","preview","unreadCount"}` |
| `inbox/get` | none | `{"status","freshness","items","error","feed","canRead"}`: the Inbox's conversations, each `{"id","channel","target","message","latestMessage","messageIds","messages","root","author","workflowOwner","preview","createdAt","mentioned","thread","unreadCount","manual","readThrough"}`. `target` and each `readThrough` mark are places in `read/mark`'s terms (`{"channel","thread"}` or `{"channel","message"}`), so marking the item read marks exactly what Buzz would; `manual` is a mark from `read/markLocal`. `feed` is the history read's `{"status","error","incomplete"}`. |
| `inbox/watch` | none | `{"watch"}`. Buzz then sends `inbox/changed` with what `inbox/get` gives, now and whenever the Inbox changes. Stop with `inbox/unwatch`. |
| `read/mark` | `all: true`, or `channel` with an optional `message`, `read` (default `true`), and `through` with an optional `root` | `null` once saved. Marks everything read, a channel or message read or unread, or a channel or thread read up to `message`. Read marks sync to your other devices; unread marks stay on this device, as in buzz-app. |
| `read/markLocal` | `channel`, and optionally `thread` (its root) or `message` | `null`, after marking it unread on this device only; it never syncs |
| `read/clearLocal` | `channel`, and optionally `thread` or `message` | `null`, after dropping an unread mark made on this device |
| `outbox/get` | `id` (optional) | One event's `{"id","kind","channel","delivery","error","acknowledged","createdAt","content","tags"}` while Buzz still holds it, or `null`; without `id`, every event still on its way. `delivery` is `sending`, `accepted`, `unknown`, `failed` or `seen` (the relay showed it back). |
| `outbox/watch` | none | `{"watch"}`. Buzz then sends `outbox/changed` with `events`, the events still on their way, now and whenever that changes; one that leaves was delivered or dismissed. Stop with `outbox/unwatch`. |
| `outbox/send` | `kind`, `content`, `tags` | `{"id"}` as soon as it is queued. Buzz signs it once, as `events/publish` does, and sends it the way it sends its own messages: retried, shown in a conversation as it goes, and followed by `outbox/get` until the relay shows it back (`outbox.send`). Needs `publish-events`. |
| `outbox/acknowledge` | `id` | `null`. Says you checked the outcome of a delivered event (`outbox.acknowledge`). |
| `outbox/supports` | `kind` | Whether events of that kind can be signed and sent here (`outbox.supports`). |
| `archives/get` | `refresh` (optional) | `{"archived"}`: the identities the community archived, which Buzz leaves out of mentions, member choices and search |
| `archives/request` | `action` (`archive` or `unarchive`), `pubkey` | `null` once the archive shows it. Allowed for one of your agents (its owner attestation is the consent) or with a relay owner or admin role. Needs `manage-channels`. |
| `sessions/create` | `title`, `parent` (optional session), `agents` (optional: your agents in this community) | `{"channel"}`: a new private work session, as Me starts, once the relay lists it with the agents added. Needs `manage-channels`. |
| `sessions/memberOf` | `pubkey` | The ids of the channels that list them as a member |
| `agents/list` | none | `{"status","error","agents","running","elsewhere","teams","types"}`: `status` is `loading`, `ready`, `error` (with `error`) or `unavailable` (no agent host), so an empty list can be told from one not read yet; the selected community's agents and every agent running on this device, each `{"pubkey","name","type","owner","relay","picture","place","config","attentionEnabled","attention","skipped","timers"}` (`place` is `local` or `hosted`; `skipped` holds attention objects that could not be read, by slug, with the `problem`; `timers` is each timer's `armedAt`, `nextDue` and `used`); agents listed as running elsewhere (`place` `elsewhere`); your teams `{"id","name","instructions","members"}`; and the agent types `{"type","title","description","hosted"}` |
| `agents/watch` | none | `{"watch"}`. Buzz then sends `agents/changed` with what `agents/list` gives, now and whenever it changes. Stop with `agents/unwatch`. |
| `activity/observed` | `agent` (optional) | `{"status","turns","typing","records","history","hasOlder","workingChannels"}`: what the community's activity feed shows of agents anywhere, of one agent or all. `turns` are `{"agent","turn","channel","at","state","requests"}`, and `records` (newest 200) the decrypted transcript frames of your own agents, `{"id","agent","kind","createdAt","receivedAt","historical","channels","frame"}`. |
| `activity/history` | `agent` (optional), `older` (optional) | `null`. Reads saved records from the archive: an agent's, older ones with `older: true`, or everyone's newest. They arrive through `activity/watch`. |
| `activity/watch` | `agent` (optional) | `{"watch"}`. Buzz then sends `activity/changed` with what `activity/observed` gives, now and whenever it changes. Stop with `activity/unwatch`. |
| `agents/dismissError` | `agent`, `message` | `null`, after hiding that failed run's error on this device |
| `agents/clearActivity` | `agent` (optional) | `null`, after forgetting what agents on this device did, of one agent or all |
| `agents/create` | `type` (a `plugin/id` key, such as `com.example.hello/greeter`), `name`, optional `config`, `picture` (https) and `carry` (`{"instructions","workspace"}`: what a shared agent brings, which the type puts where it keeps them) | The new agent. Needs `manage-agents`. |
| `agents/save` | `pubkey`, and any of `name`, `picture`, `config` (the whole config), `attentionEnabled`, `attention` (slug to attention object, or `null` to delete it) and `restart` (timer slugs to start over) | `null` once saved. Needs `manage-agents`. |
| `teams/save` | `id` (to replace one), `name`, `instructions`, `members` (agents on this device) | The team. Needs `manage-agents`. |
| `teams/remove` | `id` | `null`. Its agents stay. Needs `manage-agents`. |
| `dm/open` | `pubkeys` (1 to 8 others) | `{"channel"}`: the direct message with them, made if new. Needs `manage-channels`. |
| `channels/create` | `name`, `description`, `private`, `ttlSeconds` (optional; without it the channel is ongoing), `labels` (optional, up to 16), `setup` (optional, what a template adds: `{"canvas","agents","group","template"}`) | `{"channel"}`. With `setup`, Buzz writes the Canvas, adds the agents and files the channel in the group once the channel is confirmed, as a channel template does (`channelCreation.create` with `setup`). Needs `manage-channels`. |
| `channels/creation` | none | `{"pending","notices"}`: the channel being made now (`{"name","description","labels"}`), and setups that did not finish, each `{"id","name","error"}` (`channelCreation.snapshot`). |
| `channels/dismissNotice` | `id` | `null`. Hides a setup notice. |
| `channels/action` | `channel`, `action`: `join`, `leave`, `archive`, `unarchive`, `delete` (by an owner) or `hide` (a DM) | `null` once the relay shows it. Needs `manage-channels`. |
| `channels/addMember` | `channel`, `pubkey` (such as one of your agents) | `null` once the roster lists them. Needs `manage-channels`. |
| `channels/changeMember` | `channel`, `pubkey`, `change` (`admin`, `member`, `guest` or `remove`), `expected` (their role now: `owner`, `admin`, `member`, `guest` or `bot`) | `null` once done. Needs `manage-channels`. |
| `channels/details` | `channel` | `{"channel","version","details","canEdit","attempt"}`: `details` is `{"name","description","private","ttlSeconds","labels"}`; `attempt` is a save still `saving` or `unconfirmed`, with its `details`, or `null` |
| `channels/saveDetails` | `channel`, `version` (from `channels/details`), `details`; or `channel` and `check: true` to check an unconfirmed save | `null` once saved. A save that may have landed stays `unconfirmed` until checked. Needs `manage-channels`. |
| `canvas/read` | `channel`, `strong` (default `true`: past relay replicas, as before an edit) | The channel's Canvas `{"id","author","createdAt","content"}`, or `null` |
| `canvas/history` | `channel`, `cursor` (the `next` of the page before) | `{"revisions","next"}`: 25 revisions, newest first |
| `canvas/save` | `channel`, `content`, `expected` (the `id` it was read as, or `null` for none) | The saved revision. If the Canvas changed since, it fails with code 4 and nothing is written. Needs `send-messages`. |
| `agents/memories` | `agent` (one of your agents) | `{"entries","partial"}`: its newest memory per slug, `{"slug","body","eventId","createdAt"}` |
| `kit/get` | none | `{"status","entries","error"}`: your teams, channel templates and sidebar groups in the community, each `{"eventId","createdAt","version","deleted","value"}`, where `value` has a `type` of `team`, `template` or `groups` |
| `kit/save` | `value` (as `kit/get` gives it), `expected` (its `eventId`, or `null` for new), `deleted` | `{"eventId"}`. Needs `manage-channels`. |
| `workflows/list` | `channel` | `{"status","workflows","partial","available"}`: the channel's workflows `{"id","owner","channel","revision","createdAt","yaml"}`, and which of `definitions`, `history`, `save`, `trigger` and `delete` the community offers |
| `workflows/runs` | `channel`, `workflow` (an id), `cursor` (the `next` of the page before) | `{"status","runs","next"}`: runs newest first, each `{"id","workflow","status","currentStep","trace","startedAt","completedAt","createdAt","errorCode","errorMessage"}` |
| `workflows/save` | `channel`, `yaml`, and `workflow` to replace one | `{"eventId"}` of the command. Needs `manage-channels`. |
| `workflows/delete`, `workflows/trigger` | `channel`, `workflow` | `{"eventId"}`. Needs `manage-channels`. |
| `workflows/operations` | none | The workflow commands sent this session, each `{"eventId","workflow","channel","action","createdAt","delivery","outcome","error","run","secretHeld","webhookUrl"}`: `delivery` is `sending`, `accepted`, `unknown`, `failed` or `seen`, and `outcome` `pending`, `succeeded`, `rejected` or `unknown` |
| `workflows/takeSecret` | `eventId` | A new webhook's one-time secret, or `null` once taken |
| `workflows/dismiss` | `eventId` | `null`, after forgetting a finished command. Needs `manage-channels`. |
| `projects/list` | none | The community's repositories and projects, each `{"kind","owner","dtag","address","name","description","route","event"}`. `route` is what `ui/navigate` takes as `params` for the Projects page, `buzz.projects/projects`. |
| `projects/load` | `route` (an entity's `route`, or one naming a `tab`, `commit`, or an issue or pull request `id`) | `{"entity","repositories","unavailableRepositories","channels","item","status","commit","activity","items","truncated"}`: a project's repositories, linked channels, and the issue or pull request the route names with its comments, as Nostr events |
| `projects/home` | `channel` | `{"home","ambiguous"}`: the channel's project, or `null` |
| `projects/gitAuthorization` | `repository` (a `https://…/git/<owner>/<d>.git` URL of the community) | `{"repository","token"}`: a NIP-98 value for `Authorization: Nostr `, good on that repository's Git routes for 60 seconds, for running Git yourself |
| `agents/logProof` | `id` (the agent's log session), `pubkey` (the agent), `relayUrl` (the community's relay), `nonce` (the host's challenge) | `{"signature"}`: your proof, as the agent's owner, for one read of its harness log |
| `agents/attest` | `pubkey` (an agent key) | `{"auth"}`: your NIP-OA tag `["auth", owner, "", signature]` saying you own that agent, for an agent to attach to what it signs. Needs `manage-agents`. |
| `agents/presence` | `status` (`online` or `offline`), `pubkeys` (up to 32 of your agents) | `true` once sent on the live connection, `false` if this community cannot take it: owner presence (kind 20003) for agents Buzz does not run itself, such as hosted ones. Needs `manage-agents`. |
| `services/list` | none | The services running plugins offer, each `{"plugin","service"}`. Buzz sends `services/changed` with `services`, the same list, when a plugin that offers any starts, stops or restarts, as a buzz-app plugin's `inject` learns a service came or went. |
| `services/request` | `plugin`, `service`, `method`, `params` | The answer of the plugin that offers it, within 30 seconds |
| `services/subscribe`, `services/unsubscribe` | `plugin`, `service` | `null`. Buzz then sends `service/event` (`plugin`, `service`, `event`, `data`) each time that plugin publishes an event of it. |
| `projects/git` | `owner`, `dtag`, optional `commit` and `path` | `{"head","commits","files","readme","file","diff"}`: read with Git from the community; `file` is the file at `path` and `diff` the commit's changes. Failures are `notFound`, `denied`, `unavailable` or `invalidTarget`. |
| `threads/follow` | `channel`, `root`, and `following` (`true` or `false`), or no `following` to ask | Whether you follow the thread now |
| `reminders/list` | none | `{"status","hydrated","reminders","error"}`, each `{"id","eventId","createdAt","notBefore","status","note","message"}` with `status` `pending`, `done` or `cancelled`; `hydrated` once the relay's history was read |
| `reminders/watch` | none | `{"watch"}`. Buzz then sends `reminders/changed` with what `reminders/list` gives. Stop with `reminders/unwatch`. |
| `reminders/create` | `channel`, `message`, `notBefore` (unix seconds), optional `note`, `preview` and `author` | `null` once saved |
| `reminders/snooze`, `reminders/complete`, `reminders/cancel` | `id`, and `notBefore` to snooze | `null` once saved |
| `agents/remove` | `pubkey` | `null` once the agent and its key are gone. Needs `manage-agents`. |
| `agents/previewRetype` | `pubkey` (an agent on this device), `type` | `{"config","moved","dropped","settings"}`: the config it would start with as that type, what it carries over (`instructions`, `workspace`) or leaves, and the settings the move resets or drops |
| `agents/retype` | `pubkey`, `type`, `config` (optional; from `agents/previewRetype`, edited or not) | `{"dropped"}` once it runs as the new type, keeping its key, name, picture, communities and attention. Refused while it is answering. Needs `manage-agents`. |
| `agents/portable` | none | `{"agents","warnings"}`: identities earlier agent systems saved on this device, `{"pubkey","name","picture","relay","source"}`, that are not agents yet |
| `agents/port` | `pubkey` (from `agents/portable`), `type` | The new agent, keeping the identity's key, name and picture, in the selected community. Needs `manage-agents`. |
| `agent/createConfig` | `view` (an open `agentCreate`), `config` (an object) | `null`. The new agent will start with this config. |
| `ui/openDialog` | `title`, `id` (default `dialog`), `context` (any JSON, up to 16 KiB) | `{"view"}`. Buzz opens a dialog whose body is a view of `kind` `dialog`; its `view/open` carries `contribution` (the `id`) and `context`. Use it for what a message action or button shows next. One plugin dialog shows at a time. |
| `ui/closeView` | `view` (an open `dialog`, panel or provider dialog) | `null`, once it has closed |
| `reaction/select` | `view` (an open `reactionPicker`), `emoji` | `null`. Buzz reacts with it and closes the picker. |
| `composer/settle` | `view`, `element` (a composer that handed the plugin its draft), `sent` | `null`. With `sent: true` the composer clears; otherwise it keeps the draft to try again. |
| `composer/insert` | `composer` (from `composer/tool`, or an embedded composer's `ready`), `text` | `null`, after putting the text at the composer's caret |
| `composer/insertMentions` | `composer`, `pubkeys` (at most 32), or `mentions` (`[{"pubkey","name"}]`, to label each chip yourself, as `insertMention` does) | `null`, after putting mention chips at the caret; the people become recipients. Like the other composer writes (`composer/insert`, `composer/insertResource`, `composer/capture`), it is refused while the composer is disabled or read-only. |
| `composer/insertResource` | `composer`, `uri` (a Buzz or web link), `label` | `null`, after putting the link as one chip at the caret |
| `composer/focus` | `composer` | `null`, after putting the caret back in the composer |
| `composer/capture` | `composer` | `{"view"}`. A view of `kind` `capture` takes the composer's place, as recording a voice note does, until `composer/accept` or the user discards it (the view then closes). |
| `composer/accept` | `view` (an open `capture`), and optionally `path` (a file, up to the community's limit for its type) or `data` (base64, up to 3 MiB), `name`, `mime` and `voice` (`{"duration","waveform"}`: seconds, and up to 256 levels from 0 to 255) | `null`. Ends the capture and attaches the file, if given, to the draft; with `voice`, as a voice note that plays in the draft. |

Reads cover the selected community and channels you can read in it. They
never mark anything read. A read waits up to 10 seconds for the relay.

Any request can name `community`, the `wss://` relay of another community
you joined (see `community/list`), to reach that one instead, such as the
community a running agent works in. Watches keep the community they
started with.

A message looks like this:

```json
{
  "id": "<event id>", "channel": "<channel id>",
  "author": "<pubkey>", "authorName": "Ada",
  "content": "Hello", "createdAt": 1792200000, "createdAtMs": 1792200000000,
  "edited": false, "threadRoot": null, "replyParent": null, "replyCount": 2,
  "mentions": [], "delivery": null, "kind": 9, "audience": null,
  "signer": null, "workflowOwner": null, "participants": [],
  "reactions": [{"content": "👍", "emojiUrl": null, "count": 1, "authors": ["<pubkey>"],
                 "events": [{"id": "<reaction event id>", "author": "<pubkey>"}]}],
  "attachments": [{"url": "https://…", "kind": "image", "mime": "image/png",
    "size": 73, "name": "dot.png", "duration": null, "waveform": null,
    "voiceNote": false, "dimensions": {"width": 1, "height": 1},
    "blurhash": null, "previewUrl": null}],
  "emoji": [{"shortcode": "party", "url": "https://…"}],
  "mentionReferences": [], "sourceContent": null, "sentFromThread": null,
  "diff": null, "deliveryError": null
}
```

`mentionReferences` are people named without being notified;
`sentFromThread` (`{"root","excerpt"}`) marks a copy of a thread reply;
`diff` (`{"filePath","repoUrl","commitSha","description","truncated"}`)
describes a diff message; `sourceContent` is the text before Buzz moved
attachment links out of it.

`authorName` is `null` until Buzz knows the author. Ask `profiles/get`
for anyone you need: a profile with `loaded: false` is a placeholder, and
Buzz fetches the real one for a later call. `delivery` is set on your own
messages: `sending`, `accepted`, `unknown`, `failed` or `seen`.

`messages/send` answers once the message is safely queued, as Buzz's own
composer does. The relay can still refuse it later; a followed channel then
shows it with `delivery: "failed"`, and `outbox/get` with its `id` says how
it is going.

### `channel/changed` (notification from Buzz)

A followed channel changed. It carries the same fields as `channel/messages`,
plus the `watch` id:

```json
{"jsonrpc":"2.0","method":"channel/changed","params":{"watch":1,"channel":"<id>","messages":[],"hasMore":true,"error":null}}
```

Buzz sends it once when following starts, and again on each change. Following
ends with `channel/unwatch` or when the plugin stops.

### Errors

| Code | Meaning |
| --- | --- |
| `-32601` | Buzz has no such method. |
| `-32602` | A param is missing or not valid. |
| `1` | Nothing to act on: no community is selected, or the channel is not one you can read. |
| `2` | The relay did not answer, or the read failed. |
| `3` | Sending failed, such as when you cannot post in the channel. |
| `4` | The plugin did not declare the permission this needs. |

Toast titles are cut to 128 characters, and bodies to 200.

## Sharing a plugin

### Add from Git

**Settings → Plugins → Add from Git…** takes an HTTPS or SSH repository
(`owner/repository` means GitHub) and an optional branch or tag. Buzz clones
it and lists every folder with a `buzz-plugin.json`: the command it runs, what
it may do, and who signed it. Installing copies that folder into Buzz's own
plugin folder, and the plugin starts off.

Nothing in the repository runs while Buzz reads it. Git uses no global or
system config, runs no hooks, fetches no submodules or LFS files, asks for no
passwords, and speaks only HTTPS and SSH. Symlinks are checked out as plain
files. A clone may take 60 seconds and 256 MiB. SSH uses your SSH agent and
known hosts, not your SSH config.

A repository hosted by the selected community, including a private one, is
fetched as you: Buzz signs in with NIP-98, and sends that sign-in only to that
repository.

**Update** on an installed plugin reads its repository again and shows the
same preview. An update keeps the plugin's switch, and a running plugin
restarts. If the new version asks for more permissions, it is turned off until
you review it.

A plugin added from a folder and one installed from Git cannot share an id.
Remove one to add the other.

### Signing

Signing tells people who published a plugin. It uses buzz-app's NIP-PS
signature event (kind `1064`) over a list of the folder's files:

- `plugin.artifact.json` lists every file except hidden ones, with its size and
  SHA-256.
- `plugin.signature.json` is the signed event: empty content, an `x` tag with
  the SHA-256 of `plugin.artifact.json`, and a `t` tag of
  `buzz-plugin-folder-v1`.

**Sign** on a plugin added from a folder writes both files with your Buzz key.
Commit them with the plugin. A signed folder cannot contain symlinks.

Buzz checks the signature and every file before each start. Once Buzz has
seen a plugin signed, later versions must be signed by the same key: an
unsigned copy or another publisher stops the plugin until you remove it and
add it again. Editing a signed folder stops it too; sign it again, or delete
`plugin.signature.json` while you work on it.

A signature identifies a publisher. It does not make a plugin safe.

### Safe mode

Start Buzz with `BUZZODZ_SAFE_MODE=1`, as with buzz-app, to keep the plugins
you added from running for that launch. Their switches stay as they were.

## When something goes wrong

- Lines on stdout that are not JSON-RPC are logged and ignored. A stray
  `print` to stdout does not stop the plugin, but its output goes nowhere.
- A tree that is not valid keeps the last good one on screen and shows the
  error under it.
- If the plugin exits, its pages and panels stay and show the exit code and
  its last stderr lines, with a **Restart** button. Fixing a file in the
  folder restarts it too.
- **Settings → Plugins** shows each plugin's state. **Reload** reads the
  manifest again and restarts the plugin.

## How this differs from buzz-app

buzz-app plugins are JavaScript running inside the app; these are programs
that draw with Buzz's elements. Everything buzz-app's plugin API offers has a
request, a notification or a contribution here. Where a buzz-app plugin
passes a function, or reaches into the app, it works differently:

- **Functions become declarations or questions.** A buzz-app plugin hands
  over functions such as `matches(url)`, `eligible(channel)` or
  `describeMigration(config)`. Here a contribution declares what they would
  decide: link prefixes, regular expressions, channel kinds, config keys.
  Where only the plugin can decide, Buzz asks it (`message/render`,
  `names/resolve`, `agent/summary` and so on), or the plugin says when
  something changed (`settings/visibility`, `shortcut/enable`,
  `render/invalidate`, and `completion/publish` for a suggestion that can
  no longer be picked, in place of `canSelect`).
- **Answers come later.** Buzz draws while it waits for a plugin's answer.
  It shows the plain message, link, name or summary first, then the
  plugin's answer once it arrives, and keeps answers until what they
  describe changes.
- **Views are element trees.** A plugin draws with Buzz's elements, and can
  embed Buzz's own composer and thread. It cannot run arbitrary UI code.
- **Reading follows Buzz's views.** buzz-app's reading handle (`observe`,
  `catchUp`) marks messages read as a plugin shows them. Here only Buzz
  draws messages, so its own views do this, including a thread a plugin
  embeds; `read/mark` covers explicit marks.
- **Reads are snapshots.** Agent memories, workflows, runs and project
  reads answer once rather than handing out live views. Ask again to
  refresh, or watch the channel, thread, unread counts, Inbox, names,
  presence, events or outbox where a change needs to be seen as it
  happens.
- **Services are announced, not injected.** A buzz-app plugin `inject`s
  the services it needs and starts once they exist. Here a plugin always
  starts; `services/list` and `services/changed` say which are offered.
- **A run ends when the plugin answers.** buzz-app's `workingUntil` keeps
  a run open past its answer. Here the plugin answers, then opens a call
  as the same agent with `agent/open` and keeps acting through it.
- **Hosted agents have no unfinished state.** Making a hosted agent
  finishes or fails as a whole.
- Buzz draws a panel's channel-header launcher from its `icon` and `label`,
  as the icon button buzz-app's own launchers are; a plugin cannot draw a
  different control there.
- There is no embedded browser (`host.browser` is `false`).
- Host grants (commands, network origins, processes) do not apply: a plugin
  is a program and already has your access.

## Not built yet

- A WebAssembly runtime for plugins that run sandboxed
  ([#12](https://github.com/benthecarman/buzz-gpui/issues/12)).

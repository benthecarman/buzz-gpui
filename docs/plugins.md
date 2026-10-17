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
| `permissions` | Optional. What the plugin may do as you. `send-messages`: send, edit, react to and delete messages, and upload files to attach. `publish-events`: publish other Nostr events (see `events/publish`). `manage-agents`: create, change and remove your agents and teams. `manage-channels`: start DMs and create, join, leave, archive and delete channels. Reading needs no permission. |

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

- A contribution `id` uses lowercase letters, digits, `-` and `_`. Titles are
  1 to 64 characters.
- `icon` names a Buzz icon, such as `House`, `Globe`, `Bell`, `Gear`, `Code`
  or `Plug`. An unknown name shows the plug icon.
- `matches` lists link prefixes. A matching link clicked in a message opens
  the panel as a tab. When several panels match, the lowest `order` wins,
  then the lowest `plugin/id` key. Buzz's own panels are at 0. By convention,
  use -10 to specialise links another panel shows, 0 (the default) for your
  own links and 100 for a catch-all.
- `launcher` adds a button to the channel header. `placement` is `side` (the
  default) or `bottom`; `target` is what the panel opens showing (the
  channel's id by default). `menu` (`{"label"}`) adds an entry to the channel's
  menu; `kinds` (such as `["stream", "forum"]`) and `private` (`true` or
  `false`) limit the channels it is offered in.
- A page's `placement` is `sidebar` (the default), `topbar` (`toolbar` is the
  same here) or `hidden`. `layout` is `document` (the default, padded) or
  `workspace` (edge to edge). Buzz's dock shows a page's companion beside it. With `routes: true`, `ui/navigate` can open the page
  at `params` (a JSON object up to 4 KiB): its view's `view/open` carries
  `route`, and Buzz sends `page/route` with the `view` and `route` when it
  changes. Back and Forward step through routes.
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
| `attachmentRenderers` | `id`, `mime` (MIME type prefixes) | Draws matching audio attachments hosted by the community, in place of the audio player. Buzz asks with `attachment/render`. As in buzz-app, images, videos and files keep Buzz's own views, and voice notes keep Buzz's voice note card. |
| `inlineRenderers` | `id`, `pattern` (a regular expression, up to 500 characters, that cannot match empty text) | Draws the pieces of message text the pattern matches, such as ticket numbers. Buzz asks with `inline/render`. Mentions, emoji, links and code are left alone. |
| `composerTools` | `id`, `title`, `icon`, `order`, `reactionPicker` | A button in every composer's toolbar. Clicking it sends `composer/tool`. With `reactionPicker: true`, it can also replace the emoji picker that "Add reaction" opens: the tool with the lowest `order`, then the lowest `plugin/id` key, wins, and Buzz's own picker is `buzz.emoji/emoji` at 0. The picker is a view of `kind` `reactionPicker`. Send `reaction/select` with its `view` and an `emoji` to react. |
| `composerAccessories` | `id`, `title`, `order`, `placement`, `kinds` | With `placement` `composer` (the default): a view above every composer, of `kind` `accessory`; `view/open` also carries the composer's `channel` and `thread`. Keep it to a line or two. With `placement` `message`: something beside each message's reactions, such as work on it; Buzz asks `message/accessory`, for chat messages or the `kinds` listed. |
| `namingPolicies` | `id` (at most one) | Display names for people and agents (`identityNames.register`). Buzz asks `names/resolve` and shows public names until the answer comes. As in buzz-app, a policy applies only while it is the only one on: switch off Identity Naming in Settings → Plugins to use a plugin's. |
| `accountActions` | `id`, `title`, `icon` | A row in the account menu that opens a dialog titled `title`. The dialog's body is a view of `kind` `dialog`. Send `ui/closeView` with its `view` to close it. |
| `completions` | `id`, `title`, `trigger` (one character other than `@`, `#` and `:`), `order` | Typing the trigger at the start of a word in a composer opens the plugin's suggestions. Buzz asks with `completion/query`. When plugins share a trigger, the lowest `order`, then the lowest `plugin/id` key, answers. |
| `shortcuts` | `id`, `title`, `keys` (such as `mod+shift+k`), `order`, `allowInEditable`, `allowInModal`, `repeat` | A shortcut, listed and rebindable in Settings → Shortcuts. Pressing it sends `shortcut`. Each chord needs `mod` or `alt`; Buzz's own chords win. It does not run while a text field has focus or a dialog is open unless `allowInEditable` or `allowInModal` is set, and a held chord runs once unless `repeat` is set. |

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
  `attentionLabel` and `blocking`. Buzz sends `agentType/check` when the user
  asks to check again. The provider's dialog is a view of `kind`
  `agentProvider`; close it with `ui/closeView`.
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
- `tabs` (`[{"id", "title"}]`, at most 8): settings tabs before Attention.
    Each is a view of `kind` `agentTab`; `view/open` also carries `type`,
  `agent`, `contribution` (the tab id) and `conversation` (`{"channel","root"}`
  when the agent was opened from a message). To save, send `agent/save` with the
  `view` and the whole new `config`.

While a call or run is open, the plugin can act as its agent:

| Request | Params | Result |
| --- | --- | --- |
| `agent/tool` | `call` or `run`, `tool` (a Buzz tool, such as `send`, `read`, `react` or `channels`, or in a run one of the attention tools, such as `watch_add`), `arguments` | The tool's answer: `{"text"}` or `{"image"}` |
| `agent/step` | `run`, `tool`, `target` (optional) | `null`. Shows what the run is doing on its message, such as `read` on a channel. |
| `agent/acknowledged`, `agent/superseded`, `agent/ignored` | `run` | `null`. Marks the run's message as taken up, as replaced by a newer request (its bubble hides), or as declined (nothing shows). |
| `agents/activity` | none | What agents on this device are doing, newest first: `[{"agent","channel","message","root","state","startedAt","error","step"}]`, `state` being `waiting`, `working`, `done`, `error` or `superseded` |
| `agent/current` | `run` | Whether the trigger should still run: `false` once the agent is removed, its attention turned off, or its watch changed |
| `agent/query` | `call` or `run`, `filters` (1 to 10 NIP-01 filters) | The events, read as the agent, at most 500 |
| `agent/publish` | `call` or `run`, `kind`, `content`, `tags` | The signed event, published as the agent |
| `agent/upload` | `call` or `run`, `data` (base64, up to 3 MiB), `mime` | `{"url","sha256","size","type",…}`, uploaded as the agent |

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
| `attachment/render` | `renderer`, `attachment` (`url`, `mime`, `name`, `size`) | `{"tree"}` or `null` |
| `inline/render` | `renderer`, `text` (what the pattern matched) | `{"tree"}`, drawn in the line of text, or `null` to leave it as text |
| `names/resolve` | `policy`, `identities` (`[{"pubkey","name","isAgent","owner"}]`, at most 5,000), `viewer`, `candidates` (the keys in view, or `null` for everyone) | `{"labels": {"<pubkey>": {"name", "qualifier"}}}`. Keys left out show their public name. |
| `completion/query` | `completion`, `query` (the text after the trigger, up to the caret), `channel`, `thread` (the root the composer replies in, or `null`) | `{"items": [{"label", "detail", "text", "mentions"}]}`, at most 50. Picking one replaces the trigger and query with `text`, or with `mentions` (pubkeys) as chips that make them recipients. |

A renderer's tree can use every element but `input`. Its buttons and switches
send `render/event` with `renderer`, `target` (the URL, message or attachment
it drew), `id`, `event` and `value`.

### Other notifications from Buzz

| Notification | Params |
| --- | --- |
| `message/action` | `action`, `message` |
| `composer/tool` | `tool`, `channel`, `thread` (the root it replies in, or `null`), `composer` (an id for `composer/insert`) |
| `shortcut` | `shortcut` |
| `render/event` | `renderer`, `target`, `id`, `event`, `value` |

### Notifications from the plugin

| Notification | Params | Effect |
| --- | --- | --- |
| `message/markers` | `action`, `messages` (event ids) | Those messages show the action's icon after the byline; the list replaces the last one. |
| `page/badge` | `page`, `text` (up to 8 characters, or `null` to clear), `tone` | A badge on the page's row. |
| `shortcut/enable` | `shortcut`, `enabled` | Turns one of the plugin's shortcuts off or back on for now, as buzz-app's `when` does. |
| `settings/visibility` | `card`, `visibility` (`visible`, `hidden` or `pending`) | Lists or hides one of the plugin's Settings sections, such as one only some communities need. Sections start visible. |

### 2. `view/open` (notification from Buzz)

Each open page or panel is a view with its own id. Buzz tells the plugin when
one opens:

```json
{"jsonrpc":"2.0","method":"view/open","params":{
  "view":"v3","contribution":"forecast","kind":"panel",
  "target":"https://weather.example/today","channel":"<channel id>","thread":null
}}
```

For a page, `kind` is `page`, and `target` and `channel` are `null`. A panel
opened from the channel header has the channel id as its `target`.

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
| `composer` | `id`, `channel`, `thread` (optional root), `placeholder`: Buzz's own composer, which sends as the channel's does (only in views, not renderers) |
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
| `community/current` | none | `{"name","relay","viewer","status","live","cached"}` for the selected community, or `null`. `viewer` is your pubkey; `status` is `connecting`, `ready` or `failed`; `live` says the live connection is up; `cached` says it shows saved data while the relay has not answered. Buzz sends the notification `community/changed` with the same fields whenever any of them change, such as when another community is selected. |
| `community/list` | none | Your communities: `[{"name","relay"}]` |
| `community/retry` | none | `null`. Connects to the selected community again after a failure. |
| `community/channels` | none | The selected community's channels: `[{"id","name","description","private","archived","kind","members","labels","lastActivityAt","readOnly","creator","createdAt","parent"}]`, where `kind` is `stream`, `forum`, `dm` or `session` |
| `session/watch` | `what`: `channels`, `presence`, `emoji`, or `typing` or `activity` with a `channel` | `{"watch"}`. Buzz then sends `session/changed` with `what` and `value` (as `community/channels`, `presence/get`, `emoji/list`, `typing/get` with `channel` and `anywhere`, or `threads/activity`'s `items`), now and whenever it changes. Stop with `session/unwatch`. |
| `channel/messages` | `channel`, `limit` (default 50, at most 200) | `{"channel","messages","hasMore","error"}`, the newest messages, oldest first. For older history, use `events/query` with `until`. |
| `thread/messages` | `channel`, `root` | `{"root","replies","hasMore"}` |
| `channel/watch` | `channel`, `limit` | `{"watch","channel"}`. Buzz then sends `channel/changed` (below). |
| `thread/watch` | `channel`, `root` | `{"watch","channel","root"}`. Buzz then sends `thread/changed` with `root`, `replies`, `hasMore` and `error`, now and whenever the thread changes. |
| `events/watch` | `filters` (see `events/query`) | `{"watch"}`. Buzz then sends `events/live` with `events`, the live events that match, from now on. |
| `channel/unwatch`, `thread/unwatch`, `events/unwatch` | `watch` | `null`. A plugin follows at most 16 channels, threads and filters in all. |
| `events/query` | `filters`: 1 to 10 NIP-01 filters of `ids`, `authors`, `kinds`, `#<letter>` tags, `since`, `until` and `limit` | The matching signed events, at most 500, read from the community as you |
| `profiles/get` | `pubkeys` (hex, at most 500) | `[{"pubkey","name","picture","about","nip05","isAgent","loaded"}]` |
| `messages/send` | `channel`, `content`, `thread` (optional `{"root","parent"}`), `mentions` (optional pubkeys), `attachments` (optional, from `files/upload`) | `null`, once the message is queued. Needs `send-messages`. |
| `messages/edit` | `channel`, `message`, `content` | `null` once sent. Only your own messages. Needs `send-messages`. |
| `messages/react` | `channel`, `message`, `emoji` | `null` once sent. Needs `send-messages`. |
| `messages/delete` | `channel`, `message` (a message or reaction) | `null` once sent. Your own, or an agent's you own. Needs `send-messages`. |
| `messages/report` | `channel`, `message`, `reason` (`spam`, `profanity`, `nudity`, `impersonation`, `malware`, `illegal` or `other`), `note` | `null` once the relay accepts it. Needs `send-messages`. |
| `messages/retry`, `messages/dismiss` | `message` (one of yours whose `delivery` is `failed`) | `null`. Sends it again, or drops it. Needs `send-messages`. |
| `files/upload` | `data` (base64, up to 3 MiB), `name`, `mime` (optional; Buzz guesses) | `{"name","url","mime","size","sha256"}`, for `messages/send`'s `attachments`. Needs `send-messages`. |
| `events/publish` | `kind`, `content`, `tags` | `{"id","event"}` once the relay accepts it. Buzz signs as you, only kinds it writes itself (such as 7, 9, 30315 and 40003). Needs `publish-events`. |
| `ui/navigate` | `to`: `home`, `me`, `agents`, `settings` (with an optional `section`, such as `appearance`, `plugins` or a settings card's `plugin/id`), `page` (with `page`, a page's `plugin/id`, and optional `params` for a page with `routes`), `back` or `forward`; and optionally `community` (a joined community's `wss://` relay) | `null` |
| `ui/openPanel` | `panel` (one of the plugin's panel ids), `target` (what it shows, such as a link or any text the plugin reads from `view/open`) | `null`. Opens the panel as a tab of the open conversation. |
| `panel/canOpen` | `target` | Whether any panel, the plugin's or not, shows `target`. |
| `panel/open` | `view` (an open panel), `target` | `true` if a panel shows `target`. When it is this panel, the view shows it in place and the plugin hears `view/retarget` (`view`, `target`, `canGoBack`); otherwise the panel that shows it opens as a tab for the same conversation and thread, and this one closes. |
| `panel/push` | `view` (an open panel), `target` | As `panel/open`, but keeps this panel: within it, Buzz shows a Back button that returns to the previous target with another `view/retarget`; another panel opens beside it. |
| `ui/openLink` | `url` (`buzz://` or a web link) | `null`. Opens it as a click in a message does: in Buzz, in a panel that shows it, or in the browser. |
| `names/get` | `pubkeys` (at most 500) | `{"<pubkey>": {"name","qualifier"}}`: names as Buzz shows them, through the naming policy, with the npub suffix that tells namesakes apart |
| `ui/openConversation` | `channel`, `message` (optional), `thread` (optional), `members` (optional, `true` to open its members) | `null`. Buzz shows the conversation. |
| `presence/get` | `pubkeys` (at most 500) | `{"<pubkey>": "online" | "away" | "offline" | null}`, for people Buzz already follows; `null` for others |
| `statuses/get` | `pubkeys` (at most 20) | `{"<pubkey>": {"text","emoji","updatedAt","expiresAt"} | null}`, read fresh |
| `typing/get` | `channel`, `root` (optional, a thread) | The pubkeys typing there |
| `emoji/list` | none | The community's custom emoji: `[{"shortcode","url"}]` |
| `people/search` | `query`, `page` (from 1, the default) | `{"people","more"}`: profiles that match, as `profiles/get` gives them |
| `notifications/submit` | `category`, `sourceKey` (unique per thing; repeats are dropped), `title`, `body`, and what a click opens: an optional `channel`, `message` and `thread`, or a `page` (`plugin/id`) | `true` if it showed. Buzz applies the user's notification settings. |
| `agent/save` | `view` (an open `agentTab`), `config` (an object, the whole config) | `null` once saved |
| `unread/get` | none | `{"channels", "mentions", "direct", "observed"}`. `channels` maps each channel with something unread to `{"unread","count","attention","mentions","threads","manual","stale"}`. Counts are sampled, not exact. |
| `unread/watch` | none | `{"watch"}`. Buzz then sends `unread/changed` with the same fields, now and whenever they change. Stop with `unread/unwatch`. |
| `unread/message` | `channel`, `message` | Whether that loaded message shows as unread now |
| `unread/sync` | none | `{"capability","status","complete","error"}`: how read marks sync to the relay, or `null` before it is known. `unread/retrySync` tries again after a failure. |
| `threads/activity` | `channel` | `{"channel","items","error"}`: threads with unread replies for you, newest first, each `{"root","latestMessage","author","createdAt","preview","unreadCount"}` |
| `inbox/get` | none | `{"items","error"}`: the Inbox's conversations, each `{"channel","message","latestMessage","root","author","preview","createdAt","mentioned","thread","unreadCount"}` |
| `read/mark` | `all: true`, or `channel` with an optional `message`, `read` (default `true`), and `through` with an optional `root` | `null` once saved. Marks everything read, a channel or message read or unread, or a channel or thread read up to `message`. |
| `agents/list` | none | `{"agents","running","elsewhere","teams","types"}`: the selected community's agents and every agent running on this device, each `{"pubkey","name","type","owner","relay","picture","config","attentionEnabled","attention"}`; agents listed as running elsewhere or hosted; your teams `{"id","name","instructions","members"}`; and the agent types `{"type","title","description","hosted"}` |
| `agents/watch` | none | `{"watch"}`. Buzz then sends `agents/changed` with what `agents/list` gives, now and whenever it changes. Stop with `agents/unwatch`. |
| `agents/create` | `type` (a `plugin/id` key, such as `com.example.hello/greeter`), `name`, optional `config` and `picture` (https) | The new agent. Needs `manage-agents`. |
| `agents/save` | `pubkey`, and any of `name`, `picture`, `config` (the whole config), `attentionEnabled`, `attention` (slug to attention object, or `null` to delete it) and `restart` (timer slugs to start over) | `null` once saved. Needs `manage-agents`. |
| `teams/save` | `id` (to replace one), `name`, `instructions`, `members` (agents on this device) | The team. Needs `manage-agents`. |
| `teams/remove` | `id` | `null`. Its agents stay. Needs `manage-agents`. |
| `dm/open` | `pubkeys` (1 to 8 others) | `{"channel"}`: the direct message with them, made if new. Needs `manage-channels`. |
| `channels/create` | `name`, `description`, `private`, `ttlSeconds` (optional; without it the channel is ongoing) | `{"channel"}`. Needs `manage-channels`. |
| `channels/action` | `channel`, `action`: `join`, `leave`, `archive`, `unarchive`, `delete` (by an owner) or `hide` (a DM) | `null` once the relay shows it. Needs `manage-channels`. |
| `channels/addMember` | `channel`, `pubkey` (such as one of your agents) | `null` once the roster lists them. Needs `manage-channels`. |
| `channels/changeMember` | `channel`, `pubkey`, `change` (`admin`, `member`, `guest` or `remove`), `expected` (their role now: `owner`, `admin`, `member`, `guest` or `bot`) | `null` once done. Needs `manage-channels`. |
| `threads/follow` | `channel`, `root`, and `following` (`true` or `false`), or no `following` to ask | Whether you follow the thread now |
| `reminders/list` | none | `{"reminders","error"}`, each `{"id","createdAt","notBefore","status","note","message"}` with `status` `pending`, `done` or `cancelled` |
| `reminders/watch` | none | `{"watch"}`. Buzz then sends `reminders/changed` with what `reminders/list` gives. Stop with `reminders/unwatch`. |
| `reminders/create` | `channel`, `message`, `notBefore` (unix seconds), optional `note`, `preview` and `author` | `null` once saved |
| `reminders/snooze`, `reminders/complete`, `reminders/cancel` | `id`, and `notBefore` to snooze | `null` once saved |
| `agents/remove` | `pubkey` | `null` once the agent and its key are gone. Needs `manage-agents`. |
| `agent/createConfig` | `view` (an open `agentCreate`), `config` (an object) | `null`. The new agent will start with this config. |
| `ui/openDialog` | `title`, `id` (default `dialog`), `context` (any JSON, up to 16 KiB) | `{"view"}`. Buzz opens a dialog whose body is a view of `kind` `dialog`; its `view/open` carries `contribution` (the `id`) and `context`. Use it for what a message action or button shows next. One plugin dialog shows at a time. |
| `ui/closeView` | `view` (an open `dialog`, panel or provider dialog) | `null`, once it has closed |
| `reaction/select` | `view` (an open `reactionPicker`), `emoji` | `null`. Buzz reacts with it and closes the picker. |
| `composer/insert` | `composer` (from `composer/tool`), `text` | `null`, after putting the text at the composer's caret |
| `composer/insertMentions` | `composer`, `pubkeys` (at most 32) | `null`, after putting mention chips at the caret; the people become recipients |
| `composer/insertResource` | `composer`, `uri` (a Buzz or web link), `label` | `null`, after putting the link as one chip at the caret |
| `composer/focus` | `composer` | `null`, after putting the caret back in the composer |
| `composer/capture` | `composer` | `{"view"}`. A view of `kind` `capture` takes the composer's place, as recording a voice note does, until `composer/accept` or the user discards it (the view then closes). |
| `composer/accept` | `view` (an open `capture`), and optionally `data` (base64, up to 3 MiB), `name` and `mime` | `null`. Ends the capture and attaches the file, if given, to the draft. |

Reads cover the selected community and channels you can read in it. They
never mark anything read. A read waits up to 10 seconds for the relay.

A message looks like this:

```json
{
  "id": "<event id>", "channel": "<channel id>",
  "author": "<pubkey>", "authorName": "Ada",
  "content": "Hello", "createdAt": 1792200000, "createdAtMs": 1792200000000,
  "edited": false, "threadRoot": null, "replyParent": null, "replyCount": 2,
  "mentions": [], "delivery": null, "kind": 9, "audience": null,
  "signer": null, "workflowOwner": null, "participants": [],
  "reactions": [{"content": "👍", "emojiUrl": null, "count": 1, "authors": ["<pubkey>"]}],
  "attachments": [{"url": "https://…", "mime": "image/png", "size": 73, "name": "dot.png"}],
  "emoji": [{"shortcode": "party", "url": "https://…"}]
}
```

`authorName` is `null` until Buzz knows the author. Ask `profiles/get`
for anyone you need: a profile with `loaded: false` is a placeholder, and
Buzz fetches the real one for a later call. `delivery` is set on your own
messages: `sending`, `accepted`, `unknown`, `failed` or `seen`.

`messages/send` answers once the message is safely queued, as Buzz's own
composer does. The relay can still refuse it later; a followed channel then
shows it with `delivery: "failed"`.

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
request, a notification or a contribution here, except:

- Buzz draws a panel's channel-header launcher from its `icon` and `label`,
  as the icon button buzz-app's own launchers are; a plugin cannot draw a
  different control there.
- There is no embedded browser (`host.browser` is `false`).
- Host grants (commands, network origins, processes) do not apply: a plugin
  is a program and already has your access.

## Not built yet

- A WebAssembly runtime for plugins that run sandboxed
  ([#12](https://github.com/benthecarman/buzz-gpui/issues/12)).

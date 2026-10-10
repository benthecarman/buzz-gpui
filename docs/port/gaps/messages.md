# Messages, conversation and search: parity audit (buzz-app vs buzz-gpui)

Audit date: 2026-10-09. Read-only audit of the main checkouts (`.claude/worktrees/` ignored).

- Reference: `/home/ben/projects/buzz-app` (paths in the "buzz-app reference" column are relative to it).
- Port: `/home/ben/projects/buzz-gpui` (paths in "Port status" are relative to it).
- Slice: `src/features/messages`, `src/features/conversation`, `src/features/search`, and `src/bundled/{composer,emoji,mentions,links,link-lab,diffs,voice-notes}`. Where a search behaviour lives in `src/app/shell/*` (the only consumer of `src/features/search`), that file is cited.
- Statuses: MISSING, PARTIAL (what is missing is stated), DIFFERENT (how is stated), IN PROGRESS (live Markdown styling in the composer, being added by another agent). DONE items are counted only and are not listed.
- Sizes: S = under about a day, M = a few days, L = a large subsystem.
- Each behaviour appears once. Where it touches two areas, it is listed in the area that owns it (for example, report and "Send to channel" are under section 2, timecoded replies under section 5, membership grouping under section 1).

## Summary

| Area | MISSING | PARTIAL | DIFFERENT | IN PROGRESS | DONE |
|---|---|---|---|---|---|
| 1. Message rows, Markdown, links, embeds and diffs | 39 | 13 | 11 | 0 | 43 |
| 2. Hover actions, menus, edit/delete/report, reactions, emoji and GIF pickers | 42 | 28 | 14 | 0 | 51 |
| 3. Timeline, scrolling, unread, jump-to, threads, typing and search | 43 | 13 | 13 | 0 | 61 |
| 4. Composer, autocomplete, formatting, attachments and voice recording | 42 | 20 | 11 | 4 | 42 |
| 5. Rendered attachments, media viewer, audio/video and voice-note playback | 33 | 14 | 17 | 0 | 68 |
| **Total** | **199** | **88** | **66** | **4** | **265** |

Largest gaps, in short:

- Diff messages render as raw Markdown: there is no diff card, hunk view, file headers or expanded split/unified dialog (section 1).
- No report flow, "Send to channel", "Follow thread", "Mark read", or outbox-aware gating and recovery for edits, deletes and reactions (section 2).
- Thread rows have no hover action bar or reaction picker. There is no thread typing indicator, jump-to-latest or read tracking in threads (sections 2 and 3).
- No per-channel scroll restore, no midnight relabel, and the dock is not resizable or responsive (section 3).
- Composer: no list continuation or indent, no link dialog, no background (send-then-upload) model, a simplified mention ranking and choice set, no team mentions, no draft conflict handling. WYSIWYG styling is IN PROGRESS (section 4).
- Media: no inline video playback, no timecoded replies, markers or review reactions, no viewer animations, no copy image, no snapshot attachments (section 5).
- Link and message hover cards, channel `#name` references in prose, membership grouping and avatars, workflow bylines, and cross-block text selection are missing (section 1).

## 1. Message rows, Markdown, links, embeds and diffs

Note: Confirmed in buzz-app: there are no OpenGraph/unfurl cards and no YouTube embeds. Links get only a host icon. The only link "previews" are hover cards for Buzz message and channel links. Code blocks have no syntax highlighting or copy button. Edited messages show no "(edited)" marker.

Counts: MISSING 39, PARTIAL 13, DIFFERENT 11, IN PROGRESS 0, DONE 43.

### Message rows: kinds, grouping, byline, avatar

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Time tooltip: full date and time with time zone ("…3:05:09 PM EDT"), 500ms delay | src/features/messages/MessageTimestamp.tsx:75 | DIFFERENT (crates/app/src/ui/time.rs:69-73) | The port's tooltip omits the time zone abbreviation. Append `%Z` (or a local tz name) | S |
| Name resolution through identity naming (duplicate names disambiguated, channel-member-scoped `useChannelIdentityNames`) for bylines, mentions and membership rows | src/features/messages/MessageRow.tsx:227-230 | MISSING (crates/app/src/ui/message.rs:79-85) | The port uses the raw profile name. Port `identity-names/policy.ts` and use it in bylines, mentions, membership text and thread avatars | M |
| Avatar: 40px circle for people, squircle for agents (agent envelope, known agent key, or `isAgent`), initial fallback | src/features/messages/MessageRow.tsx:245-248, 444-470 | PARTIAL (crates/app/src/ui/message.rs:223-237) | The port checks the envelope and `profile.is_agent` only. Also treat keys in the viewer's agent library (`agentPubkeys`) as agents | S |
| Clicking the avatar opens the author's profile (round IconButton, "View {name} profile") | src/features/messages/MessageRow.tsx:405-443 | MISSING (crates/app/src/ui/message.rs:223-237) | Make the head avatar clickable and call the same profile opener mentions use (`navigation::show_profile`) | S |
| Presence badge on agent avatars (online, away, offline dot with cutout) | src/features/messages/MessageRow.tsx:249-254, 434 | MISSING (crates/ui/src/components/avatar.rs:101 supports it; message.rs never sets it) | Read agent presence from the session and pass `.presence(...)` on agent avatars in rows | S |
| Agent "thinking" badge on the avatar while the agent works in the channel | src/features/messages/MessageRow.tsx:424-433 | MISSING | Show the thinking pill (three pulsing dots) on agent avatars while their typing state is active | S |
| Workflow message avatar: squircle with a lightning-bolt icon | src/features/messages/MessageRow.tsx:391-404 | MISSING (crates/app/src/ui/message.rs:223-237) | When `row.workflow_owner` is set, draw `Avatar::fallback_icon(Lightning, 20)` in a squircle in place of the signer's avatar | S |
| Workflow byline: "Workflow · owned by {owner}", with the owner as a link button that opens their profile | src/features/messages/WorkflowByline.tsx:31-46 | MISSING (crates/app/src/ui/message.rs:256-279) | Render the workflow byline when `workflow_owner` is set (notifications already use the wording at crates/app/src/notifications.rs:188) | S |
| Workflow info popover: "Workflow message" with an explanation, owner name, owner npub, relay signer npub | src/features/messages/WorkflowByline.tsx:47-76 | MISSING | Add an xs info IconButton that opens a popover with these fields | S |
| "Sent from Thread — {excerpt}" line above the body | src/features/messages/MessageRow.tsx:613-629 | PARTIAL (crates/app/src/ui/message.rs:280-294) | The port shows link-coloured text that does nothing. Make it a real thread link (ChatsCircle icon, opens `buzz://message?channel&id=root&thread=root`) | S |
| Thread-summary participant avatars have name tooltips and agent squircles from the agent library | src/features/messages/ReplySummary.tsx | PARTIAL (crates/app/src/ui/message.rs:952-971) | Add the `title` tooltips and agent-library squircles | S |

### System / membership rows

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Group consecutive membership events (same day, gaps under 1 hour, compatible kinds, join-then-leave lifecycle) into one row | src/features/messages/membership-rows.ts:22-70 | MISSING (crates/app/src/ui/message.rs:203) | Port `membershipRows`. The port renders one line per event | M |
| Wording: "joined", "{A} was added by {X}", "were added", "arrived", "joined and left the channel", "left the channel", "was removed by {X}", ", along with B, C and N others" | src/features/messages/membership-rows.ts:72-131 | PARTIAL (crates/app/src/ui/message.rs:1016-1032) | The port says "joined" even when someone else added the person, and has no group wording. Port `membershipDescription` in full | S |
| Viewer named "You"/"you" ("You were added by X") | src/features/messages/membership-rows.ts:84-88 | MISSING (crates/app/src/ui/message.rs:204) | Use "You"/"you" when the target or actor is the viewer | S |
| Up to 3 stacked avatars (agent squircles) plus a "+N" chip | src/features/messages/MembershipRow.tsx:42-72 | MISSING | Add the 28px avatar stack with a 2px panel ring and the +N chip | S |
| Tooltip listing every named person in the group | src/features/messages/MembershipRow.tsx:73 | MISSING | Add a tooltip with the `Intl.ListFormat`-style list of names | S |

### Delivery state (pending / failed)

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| 10-second grace period: nothing shows for the first 10s. After that, a message still Sending or Accepted (not Seen) shows "Delivery not yet confirmed." | src/features/messages/delivery.ts:3-13, src/features/messages/DeliveryNotice.tsx:14-22 | PARTIAL (crates/app/src/ui/message.rs:315-319) | The port shows Failed/Unknown at once and never warns about a stuck Sending/Accepted row. Add the grace timer and include Sending/Accepted after 10s | S |

### Markdown constructs

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Rich inline content inside table cells (clickable links with icons, mentions, custom emoji, nested code) | src/features/messages/MessageMarkdown.tsx:543-602 | PARTIAL (crates/app/src/ui/markdown.rs:436-466, 902-924) | Cells are flat `StyledText`: links are only coloured and cannot be clicked, mentions and custom emoji are not rendered. Render cells through the same inline `TextView` path | M |
| GFM footnotes (reference marker plus a footnotes section) | src/features/messages/MessageMarkdown.tsx:620 (remark-gfm) | DIFFERENT (crates/app/src/ui/markdown.rs:317) | The port prints `[^id]` as text and drops the definitions. Render the footnote marker and an end section, or match whatever react-markdown shows | S |
| Formatting inside link labels kept (`[**bold**](url)`) | src/features/conversation/MessageLink.tsx:207-220, src/bundled/links/InlineLink.tsx:152-158 | PARTIAL (crates/app/src/ui/markdown.rs:301, 979) | The port flattens labels to plain text. Keep emphasis/code styles in the link chip | S |
| Repair pasted wrapped links (`[label]\(url\)`, `[label]([url](url))`) before parsing | src/features/messages/message-link-parts.ts:147-165 | MISSING | Port `normalizeWrappedLinks` into body preparation | S |
| Custom emoji tooltip (`title=":shortcode:"`) | src/bundled/emoji/CustomEmoji.tsx:23 | MISSING (crates/app/src/ui/markdown.rs:655-668) | Add a tooltip with `:shortcode:` on inline and jumbo custom emoji | S |
| Spoiler mask animation: three tiled dot fields that twinkle (reduced motion: static) | src/features/messages/Messages.module.css:1855-1933 | DIFFERENT (crates/app/src/ui/markdown.rs:1216-1221) | The port uses a flat 35% subtle fill. Draw the dot pattern with its twinkle | S |
| Spoilers hold rich content and span formatting (`\|\|a *b* [link](u)\|\|`), and long spoilers wrap | src/features/messages/remark-spoilers.ts:44-82 | PARTIAL (crates/app/src/ui/markdown.rs:394-403, 1205-1211) | The port only finds spoilers inside one text node, flattens the inside to plain text and sets `nowrap`. Build spoilers from sibling nodes and allow wrapping | M |

### Mentions and channel references in prose

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Agent-library names also bind mentions | src/features/messages/profile-mentions.ts:27-33 | PARTIAL (crates/app/src/ui/markdown.rs:177-189) | The port reads names from profiles only. Add the viewer's agent identities | S |
| Clicking a mention opens the profile panel (side panel) | src/features/messages/MessageMarkdown.tsx:527-533 | DIFFERENT (crates/app/src/ui/markdown.rs:1119-1122, crates/app/src/ui/workspace.rs:826-835) | The port shows a profile card popover at the pointer. Decide whether to match the panel | M |
| `#channel-name` in prose becomes a channel link (hash or lock icon) for known, unarchived, unambiguous channels | src/features/messages/ReferenceText.tsx:111-187, src/features/messages/message-references.ts:14-72 | MISSING (crates/app/src/ui/markdown.rs:337-416) | Port `messageReferences` for channels into the prose scan and emit `buzz://channel/<id>` links | M |

### Links: inline rendering and behaviour

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Host icon table: GitHub, Drive, Docs/Sheets/Slides, Figma, Notion, Slack, Dropbox, OneDrive, GitLab, YouTube, Loom, Zoom, Teams, globe | src/bundled/links/InlineLink.tsx:33-102 | PARTIAL (crates/app/src/ui/markdown.rs:1235-1270) | Missing: OneDrive (`onedrive.live.com`, `1drv.ms`, `*-my.sharepoint.com`), `dropboxusercontent.com`/`db.tt`, `zoom.com`, `teams.cloud.microsoft`. Other `docs.google.com` paths should use the Drive icon, not FileText | S |
| Raw URLs are cut to 44 characters plus "…" (only when longer than 45) | src/bundled/links/InlineLink.tsx:160-163 | DIFFERENT (crates/app/src/ui/markdown.rs:1174-1179) | The port truncates at more than 44, so a 45-character URL loses its last character. It also compares the label to the URL literally, so Unicode-encoded destinations are not treated as raw | S |
| Long link labels wrap; only the icon and the host stay together | src/bundled/links/InlineLink.tsx:164-177, src/shared/InlineReference.module.css:12-14 | PARTIAL (crates/app/src/ui/markdown.rs:1090) | The whole chip is `whitespace_nowrap`, so long authored labels overflow. Split the lead from the rest and let the rest wrap | M |
| Bare `buzz://` URLs in text become Buzz links (trailing punctuation trimmed) | src/features/messages/message-link-parts.ts:86-144 | MISSING (crates/relay/src/fold/attachments.rs:476) | GFM autolink literals only cover http/www/email. Add the `messageLinkParts` scan for bare `buzz://` (and `<buzz://…>` already works) | S |
| Unresolved Buzz links: look up channels the reader hasn't joined (`describe`/`refer`) for their name; "Private channel" with a lock only when access is withheld; otherwise generic "Channel" / "Message" / "Thread" / "Buzz" | src/features/conversation/MessageLink.tsx:95-118, src/features/conversation/channel-reference.ts:13-41, src/bundled/links/InlineLink.tsx:134-140 | DIFFERENT (crates/app/src/ui/markdown.rs:1141-1145) | The port says "Private channel" for every channel it doesn't list, including public ones and message links. Add the channel-reference lookup and the generic labels | M |
| DM channel links use the channel/lock icon, not a chat icon | src/bundled/links/InlineLink.tsx:151 | DIFFERENT (crates/app/src/ui/markdown.rs:1135-1137) | Port shows ChatCircle for a DM channel link. Use the lock icon (private) as the reference does | S |
| URL tooltip on plain links (`title=url`); "Private channel. You aren’t a member, or it no longer exists." on withheld links | src/features/conversation/MessageLink.tsx:228-234 | MISSING (crates/app/src/ui/markdown.rs:1080-1097) | Add the tooltips | S |
| Middle-click a Buzz link opens it in-app | src/features/conversation/MessageLink.tsx:190-205 | MISSING | Handle the middle mouse button on Buzz link chips | S |
| Status "This Buzz link couldn’t be opened here." when navigation fails | src/features/conversation/MessageLink.tsx:367-369 | MISSING (crates/app/src/ui/workspace.rs:796-799, 813) | The port only logs. Show the inline status (or a toast) | S |
| External link right-click / ContextMenu / Shift+F10 menu: "Open in browser", "Preview snapshot" (agent snapshots), "Copy link" with "Link copied" toast or error toast | src/features/conversation/MessageLink.tsx:246-301, 127-144 | MISSING | Add a context menu on link chips with these items and toasts | M |
| Session link chip: "Session · {id8}" with a file icon for session channels the viewer belongs to | src/features/conversation/MessageLink.tsx:145-159, 209-215; src/features/conversation/LinkPreview.module.css:1-27 | MISSING | Detect the session label form and draw the chip | S |
| Join/invite links render as "Join {host}" (port-only behaviour) | n/a | DIFFERENT (crates/app/src/ui/markdown.rs:1154-1168) | buzz-app has no such chip in message rendering. Keep or remove on purpose | S |

### Link hover previews and embeds

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Message link hover/focus card (above the link): author avatar (workflow bolt), name, relative time with full-date tooltip, channel icon and name (DM participants, "Notes to self", "Channel unavailable"), excerpt clamped to 4 lines that stops at a blank paragraph, "Attachment"/"Empty message" | src/features/conversation/BuzzLinkPreview.tsx:21-175, src/features/conversation/message-preview-text.ts:2-8 | MISSING | Add a hover card on message links that opens a bounded exact thread read for the target | M |
| Message preview loading and error states: "Loading message…", "Message preview unavailable." | src/features/conversation/BuzzLinkPreview.tsx:42-46, 99-104 | MISSING | Part of the hover card | S |
| Clicking the preview card opens the link | src/features/conversation/MessageLink.tsx:330-341 | MISSING | Part of the hover card | S |
| Channel link hover card: name, "Public/Private channel · Archived · Joined/Not joined · N members", description | src/features/conversation/BuzzLinkPreview.tsx:180-212 | MISSING | Add a hover card for channel-only links using the channel-reference lookup | M |
| Embedded thread (a plugin page hosting a full thread panel, with panel and media-review overlays) | src/features/conversation/EmbeddedThread.tsx:45-180 | MISSING | Plugin page API only. Needed only if plugin pages are ported | L |

### Diff messages

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Diff card: header with file path (or "Diff"), short commit SHA linking to `{repo}/commit/{sha}` (or "Source"), expand button | src/bundled/diffs/DiffMessage.tsx:21-24, 43-66 | MISSING (crates/app/src/ui/message.rs:295-304 renders the patch as Markdown) | Render rows with `row.diff` as a diff card (DiffMeta is already folded at crates/relay/src/fold/mod.rs:82-88) | M |
| Diff description line under the header | src/bundled/diffs/DiffMessage.tsx:67-69 | MISSING | Part of the diff card | S |
| Scrollable preview (max 400px) of unified hunks: green insert and red delete lines and gutters, mono font | src/bundled/diffs/DiffMessage.tsx:70-77, src/bundled/diffs/Diffs.module.css:57-96 | MISSING | Port a unified diff view (hunk parse plus line colouring) | M |
| Per-file headers in multi-file or non-modify patches: path (or "old → new"), "New file/Copied/Deleted/Modified/Renamed", +N / −N counts | src/bundled/diffs/DiffViewer.tsx:51-58, src/bundled/diffs/parse.ts:85-103 | MISSING | Part of the diff view | S |
| Strict patch validation; falls back to a raw scrollable `pre` for incomplete, binary, unknown or over-100k patches | src/bundled/diffs/parse.ts:3-83, src/bundled/diffs/DiffViewer.tsx:17-29 | MISSING | Port `parsePatch` and `consumesPatch` | M |
| Empty states: "No diff content", "No textual hunks in this diff." | src/bundled/diffs/DiffViewer.tsx:30, 78 | MISSING | Part of the diff view | S |
| Truncated notice: "Diff truncated. View full diff at the source repository" (link when a source is known) | src/bundled/diffs/DiffMessage.tsx:28-39 | MISSING | Part of the diff card | S |
| Expanded diff dialog with a Unified/Split toggle, "Close diff" | src/bundled/diffs/DiffMessage.tsx:80-108 | MISSING | Add the dialog and a split (side-by-side) layout | M |

### Copy and selection

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Select across paragraphs, blocks and several message rows in one drag | src/features/messages/selection-copy.ts:344-402 | MISSING (crates/app/src/ui/markdown.rs:927-945) | Each paragraph is a separate `TextView`. A shared selection across a row and across rows is needed | L |
| Code blocks, tables, plain-text (over-limit) bodies and jumbo emoji are selectable | src/features/messages/Messages.module.css:1386-1438 | MISSING (crates/app/src/ui/markdown.rs:52-58, 798-816, 849-900) | These are plain `div`/`StyledText`, not selectable. Render them in selectable text views | M |
| Rich clipboard: `text/html` flavour, and a Markdown flavour on paste into the composer that keeps mentions, links and code blocks | src/features/messages/selection-copy.ts:186-230, 365-388 | MISSING | Write HTML (and Markdown) along with plain text when copying from messages | M |
| Plain-text copy rules: links as "label (url)", mentions as "@Name", custom emoji as `:shortcode:`, bylines/timestamps/screen-reader text left out | src/features/messages/selection-copy.ts:144-176, src/bundled/emoji/copy-emoji.ts:7-33 | PARTIAL (crates/app/src/ui/markdown.rs:1000) | Inline objects copy their visible text, so labeled links lose the URL. Copy links as "label (url)" | S |
| "Copy link" gives `buzz://message?channel=…&id=…&thread=…` (thread kept for replies) | src/features/messages/message-copy.ts:51-78 | PARTIAL (crates/relay/src/links.rs:104-110, crates/app/src/ui/channel.rs:419-422) | The port omits `thread=` for replies. Add the thread root | S |

### Plugin rendering contracts and misc

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Message renderer registry (a plugin replaces a message body; diffs use it) | src/features/conversation/MessageBody.tsx:7-38, src/features/conversation/service.tsx:33 | MISSING | No plugin system. Diff rendering can be hard-coded (see Diff messages) | L |
| Inline text renderer registry (custom emoji are an inline plugin in buzz-app) | src/features/conversation/InlineText.tsx:13-74 | DIFFERENT (crates/app/src/ui/markdown.rs:387-393) | Custom emoji are native in the port, which is enough unless third-party inline plugins are wanted | L |
| Link renderer registry (bundled links plugin, lowest order wins, catch-all for web) | src/features/conversation/MessageLink.tsx:49-52, src/bundled/links/index.tsx:7-16 | DIFFERENT (crates/app/src/ui/markdown.rs:1070-1190) | Native in the port. A registry is needed only if plugins are ported | L |
| Attachment renderer registry (voice notes) | src/features/conversation/AttachmentView.tsx:10-37 | DIFFERENT (crates/app/src/ui/message.rs:712-727) | Voice notes are native in the port | M |
| A plugin that throws falls back to the host rendering | src/features/conversation/ContributionBoundary.tsx:14-25 | MISSING | Needed only with a plugin system | S |
| Link Lab page (visual fixture for link chips; not in buzz-app's default plugin catalog) | src/bundled/link-lab/index.tsx:38-164 | MISSING | Optional dev-only page; nothing user-facing | S |

## 2. Hover actions, menus, edit/delete/report, reactions, emoji and GIF pickers

Note: buzz-app has no right-click context menu on messages, no pin action, and no rename/delete in custom emoji settings. Message editing happens in the composer ("Editing message" mode), not inline in the row. The port does the same.

Counts: MISSING 42, PARTIAL 28, DIFFERENT 14, IN PROGRESS 0, DONE 51.

### Hover action bar: buttons, tooltips, visibility

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Action bar also appears when keyboard focus is inside the row (Tab into the row) | src/features/messages/useFloatingActionBar.ts:108-111, 36-43 | MISSING | Rows cannot take focus and there is no keyboard path to the bar. Add focusable rows (or a row-focus model), show the bar on focus, and Tab into its buttons. | M |
| Action bar on thread-panel rows (reactions, reply, copy link, more menu) | src/features/messages/MessageRow.tsx:500-574 (same row component in threads) | PARTIAL (crates/app/src/ui/thread.rs:319-336, 446-449, 858-877) | The thread panel passes no `on_hover`/`on_popup`/`on_command`, so it has no action bar. It has only a group-hover "Reply" button. Give thread rows the same bar, more menu and picker as the channel. | M |
| Quick reaction label/pressed state: "Remove X" and pressed when the viewer already reacted with it, else "React with X" | src/features/messages/MessageReactions.tsx:152-165 | PARTIAL (crates/app/src/ui/message.rs:401) | The tooltip is always "React with X" and there is no selected state. Show "Remove X" and `.selected(true)` when the viewer's reaction exists. | S |
| Quick reaction glyph lift on hover (translateY -1px, scale 1.12) and press (0.96), 120ms | src/features/messages/Messages.module.css:1601-1626 | MISSING | Add a hover/press transform animation to the quick-reaction glyph. | S |
| Quick reactions hidden when the row is 260px wide or narrower | src/features/messages/Messages.module.css:1829-1830 | MISSING | Hide the quick-reaction buttons when the row width is 260px or less. | S |
| Quick reactions and picker disabled while a reaction to this message is pending/failed in the outbox | src/features/messages/MessageReactions.tsx:67-72, 98, 163 | MISSING | Track pending kind-7/kind-5 reaction ops per message. Disable quick reactions, pills and add-reaction while one is in flight (stops double toggles). | S |
| Reactions/add-reaction hidden for read-only, archived or unlisted channels or when the outbox lacks kind 7/5 (`canReact`) | src/features/messages/MessageRow.tsx:260-269 | MISSING | Rows always get `on_react` (channel.rs:500). Gate reactions on `!read_only && !archived` and on channel membership. | S |
| "Reply" button opens/replies in the thread, including on thread replies (opens the root) | src/features/messages/MessageRow.tsx:504-514 | PARTIAL (crates/app/src/ui/message.rs:416-425) | Shown only when `thread_root_id` is none. Also show it on replies and open their root with reply intent. | S |
| "Reply" disabled with tooltip "Reply unavailable for this message" (undelivered, archived, read-only, unlisted) | src/features/messages/MessageActionBar.tsx:136-150; MessageRow.tsx:515-521 | MISSING | Add a disabled state and that tooltip. | S |
| "Copy link" button; disabled with tooltip "Message link unavailable" when no link (undelivered) | src/features/messages/MessageActionBar.tsx:151-160 | DIFFERENT (crates/app/src/ui/message.rs:426-433) | The port hides the button instead of disabling it. Render it disabled with the "Message link unavailable" tooltip. | S |
| Copy failure toast "Couldn’t copy. Try again from the message menu." (error, no timeout) | src/features/messages/MessageActionBar.tsx:114-118 | DIFFERENT (crates/app/src/ui/channel.rs:421) | GPUI's clipboard write cannot fail, so there is no error path. Nothing to do unless a fallible clipboard path is added. | S |
| Plugin-contributed markers in the byline (e.g. reminder marker) | src/features/messages/MessageRow.tsx:599-610 | MISSING | There is no message-action plugin registry. Add one if plugin actions are ported. | M |

### More menu items and their conditions

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| "Mark unread" / "Mark read" toggle (mail / mail-opened), label based on the message's current unread state | src/features/messages/MessageManagement.tsx:226-267 | PARTIAL (crates/app/src/ui/message.rs:485-492; channel.rs:428-437) | The menu always says "Mark unread". Read the message's attention state and offer "Mark read" (`session.mark_message_read`, relay session/mod.rs:450) when it is unread. | S |
| Read-state, edit and delete gated on a managed row: writable (not read-only) channel, not membership, not diff, delivered | src/features/messages/MessageManagement.tsx:210-224 | PARTIAL (crates/app/src/ui/message.rs:468, 485) | The port checks only "delivered". Add the read-only, diff and membership gates. | S |
| Mark read/unread error shown as alert "Could not update unread state. Try again." | src/features/messages/MessageManagement.tsx:254-259, 194 | MISSING (crates/app/src/ui/channel.rs:433) | The port only logs a warning. Surface the error in the page alert or a toast. | S |
| "Copy message" copies Markdown with bound mentions as `[@name](nostr:…)` links (not inside code), then attachment URLs | src/features/messages/message-copy.ts:8-49 | PARTIAL (crates/app/src/ui/channel.rs:649-656) | The port copies raw `content` and attachment URLs. Add the mention-to-link rewrite with the literal-context (code) exclusion. | S |
| "Send to channel" (own thread replies only; writable channel; outbox supports kind 9) with toast "Sending to channel" / error toast | src/features/messages/MessageActionBar.tsx:227-249; MessageRow.tsx:522-538 | MISSING | The relay has the draft builder (crates/relay/src/messages.rs:584 `sent_from_thread`) but no session API and no menu item. Add `Session::send_to_channel`, the menu item and the toasts. | M |
| "Edit message" (pencil 14px): own, not archived, editable per `lastEditableMessage` (outbox supports 40003, not agent envelope/diff, no pending edit/delete) | src/features/messages/MessageManagement.tsx:306-307, 320-339; useMessageEdit.ts:13-39 | PARTIAL (crates/app/src/ui/message.rs:499-506) | Shown for any own delivered row. Add the archived, agent-envelope, diff and pending-edit/delete checks. | S |
| Edit item disabled while a delete or edit of the row is in flight | src/features/messages/MessageManagement.tsx:322-331 | MISSING | Disable it when the outbox holds a kind-5/40003 op for this id. | S |
| "Delete message" (trash 14px): own non-archived, or an agent the viewer verifiably owns (owner authorization); outbox supports kind 5 | src/features/messages/MessageManagement.tsx:293-315, 340-368 | PARTIAL (crates/app/src/ui/message.rs:507-509) | Agent-owner deletion is missing. The relay accepts `AgentOwner` (relay messages.rs:761-799), but the app never passes it. Add the agent-ownership lookup and pass the authorization. Hide the item in archived channels. | M |
| Delete item tone | src/features/messages/MessageManagement.tsx:340-367 | DIFFERENT (crates/app/src/ui/message.rs:508) | buzz-app uses a plain menu item. The port uses `.danger()` (red). Drop `.danger()`. | S |
| Delete item disabled while an edit/delete of the row is in flight | src/features/messages/MessageManagement.tsx:341-351 | MISSING | Add outbox-pending gating. | S |
| "Follow thread" / "Unfollow thread" (bell / bell-off); hidden in DMs; error "Could not save thread follow. Try again." | src/features/messages/MessageManagement.tsx:369-387 | MISSING | The relay has `Unread::follow` (crates/relay/src/unread.rs:2546) and `threads/follows.rs`, but no session API or menu item. Add both. | M |
| Plugin actions in the menu (e.g. "Remind me") and the dialogs they open | src/features/messages/MessageRow.tsx:279-295, 323-334, 472-480 | MISSING | Needs a message-action contribution registry. | M |
| "Report" (flag) on any delivered, non-membership message | src/features/messages/MessageRow.tsx:307-322 | MISSING | Add the menu item (see Report dialog section). | S |
| Menu item order: read state, Copy, Send to channel, Edit, Delete, Follow, plugin, Report | src/features/messages/MessageActionBar.tsx:217-250; MessageRow.tsx:559-573 | PARTIAL (crates/app/src/ui/message.rs:484-509) | The order of the existing items matches. The missing items need to go in these positions. | S |
| Menu closes on outside press; Escape closes it; arrow-key navigation and Enter activation; focus returns to trigger | src/features/messages/MessageActionBar.tsx:161-210 (Base UI Menu) | PARTIAL (crates/app/src/ui/message.rs:510-513; crates/ui/src/components/menu.rs) | An outside press closes the menu. There is no Escape, no arrow/Enter keyboard navigation and no focus return. | S |

### Message edit mode (in the composer, channel and thread)

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Edit header: pencil icon, "Editing message", X button "Cancel edit" ("Close edit" once submitted) | src/features/messages/MessageComposer.tsx:1409-1421 | PARTIAL (crates/app/src/ui/composer.rs:1615-1636) | The "Close edit" label for the locked state and disabling while busy are missing. | S |
| Up arrow in an empty composer edits the newest editable own message | src/features/messages/MessageComposer.tsx:1540-1560 | PARTIAL (crates/app/src/ui/composer.rs:2609-2619; channel.rs:371-392) | It works, but the check skips pending edit/delete ops in the outbox and the outbox's support for kind 40003. Not wired in the thread panel. | S |
| Saving empty text opens the delete confirmation (or errors "Keep attachment links unchanged. To remove this message, use Delete message." when it has attachments) | src/features/messages/MessageComposer.tsx:993-1002 | MISSING (crates/app/src/ui/composer.rs:411) | Save is disabled when the text is empty. Route an empty save to the delete dialog, or show the attachment error. | S |
| Edit validation: attachment links must stay unchanged ("Keep attachment links unchanged. Only message text can be edited here.") | src/features/messages/message-edit.ts:42-61 | MISSING | Add the `validateMessageEdit` check before `session.edit`. | S |
| Edit conflict: "This message changed while you were editing. Copy your changes, then cancel and reopen it." | src/features/messages/useMessageEdit.ts:105-110 | MISSING | Compare the current row's source with the target's source on save. | S |
| Target disappears: error "This message is no longer available to edit." | src/features/messages/useMessageEdit.ts:101-104 | DIFFERENT (crates/app/src/ui/channel.rs:305-314) | The port cancels the edit silently when the row leaves the window. buzz-app keeps the text and shows an error. | S |
| Mentions in the edited text become explicit `[@name](nostr:npub)` links (`messageEditText`) | src/features/messages/message-edit.ts:24-39 | PARTIAL (crates/app/src/ui/composer.rs:1327-1360) | Only channel references become chips. Convert bound profile mentions as well. | S |
| Edit stays locked while sending; "Your edit is still in the outbox." + "Retry edit" on failed/unknown delivery | src/features/messages/useMessageEdit.ts:60-81, 128-137; MessageComposer.tsx:1698-1708 | MISSING (crates/app/src/ui/channel.rs:332-356) | The port awaits `session.edit` and closes the edit or shows the page error. Add the outbox-backed locked/retry state. | M |
| Pending/failed edit and delete recovery panel: "Message edit/deletion: …", edit content, "This may already have reached the conversation…", "Retry message update", "Discard failed update" / "Dismiss unconfirmed update" | src/features/messages/MessageManagement.tsx:484-551 | MISSING | The relay has `retry`/`dismiss` (crates/relay/src/outbox.rs:849, 891; session/mod.rs:418-426), but nothing in the UI lists failed kind-40003/5 ops per channel. | M |
| Editing in a thread panel | src/features/messages/ThreadPanel.tsx:1082-1095, src/features/messages/MessageComposer.tsx:202-203 | MISSING (crates/app/src/ui/thread.rs:259-275 always sends a new reply and ignores `submission.editing`) | Route edit submissions to `session.edit` and wire the Edit action and Up arrow in threads. | M |
| A mention picked during an edit becomes a literal identity link `[@Name](nostr:npub…)` and adds no recipient | src/features/messages/MessageComposer.tsx:783-786, 859-860, src/features/messages/message-edit.ts:19-20 | DIFFERENT (crates/app/src/ui/composer.rs:919-921) | The port inserts an `@Name` chip, which is saved as plain `@Name` text. Insert the profile link Markdown during edits. | S |
| "Finish or cancel your current edit first." / "This message is no longer available to edit." | src/features/messages/MessageComposer.tsx:729-737 | PARTIAL (crates/app/src/ui/composer.rs:336-339, crates/app/src/ui/channel.rs:306-314) | The port quietly retargets the edit, and cancels it when the row disappears. Add the messages. | S |

### Delete confirm dialog

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Pending state: "Deleting…", buttons disabled, dialog stays until accepted/seen | src/features/messages/MessageManagement.tsx:413-427, 463-468 | MISSING (crates/app/src/ui/channel.rs:446-461) | The port closes at once and runs `remove` in the background. Keep the dialog open while sending. | S |
| Failed/unknown delivery: alert with the op error or "Delivery could not be confirmed. Retry sends the same operation." and a "Retry" button; Cancel becomes "Close" | src/features/messages/MessageManagement.tsx:415-416, 444, 464-478 | MISSING | Errors go to the page banner. Add retry through the outbox and the Close label. | S |
| Guard: "This conversation is no longer available for changes." when the channel is archived/read-only/gone | src/features/messages/MessageManagement.tsx:431-443 | MISSING | Check the channel state before removing. | S |
| Dialog dropped if the channel becomes unavailable; focus returns to the composer after a delete | src/features/messages/MessageManagement.tsx:113-122, 459 | MISSING | Close the dialog on channel change and restore focus to the composer. | S |
| Escape / scrim click cancels the dialog | src/features/messages/MessageManagement.tsx:455-460 (AlertDialog onClose) | PARTIAL (crates/app/src/ui/channel.rs:789-815) | Only the Cancel button closes it. Wire Escape and scrim dismissal. | S |

### Report dialog

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Report dialog: flag + "Report message" title and description "Reports go to this community's moderators for review. The author is not notified of who reported them." | src/features/messages/ReportMessageDialog.tsx:67-81 | MISSING | Add the dialog. The relay has the kind-1984 builder (crates/relay/src/messages.rs:887) but no session publish API. Add `Session::report` (direct publish, 10s deadline). | M |
| Reason radio group, in order: Spam, Profanity or hate speech, Nudity or sexual content, Impersonation, Malware or scam, Illegal content, Other | src/features/messages/ReportMessageDialog.tsx:12-20, 107-118 | MISSING | Radio group (ReportType exists in relay messages.rs). | S |
| "Additional context (optional)" textarea, placeholder "Add anything that helps moderators..." | src/features/messages/ReportMessageDialog.tsx:119-127 | MISSING | Textarea field. | S |
| Cancel / "Submit report" (prominent, disabled until a reason, loading while pending, dialog cannot close while pending) | src/features/messages/ReportMessageDialog.tsx:72, 82-95 | MISSING | Buttons and the pending state. | S |
| Error "Failed to submit report. Try again." on the Reason field | src/features/messages/ReportMessageDialog.tsx:51-53, 107 | MISSING | Error display. | S |
| Success toast "Report submitted to community moderators" (5s) | src/features/messages/MessageRow.tsx:490-497 | MISSING | Toast on success. | S |
| Submitted report survives leaving the conversation: "Submitting report…" status or error + "Review report" | src/features/messages/ReportMessageDialog.tsx:40-65 | MISSING | Keep the in-flight report owned outside the row. | S |

### Reaction chips

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Count roll animation (260ms vertical roll on change) | src/features/messages/AnimatedReactionCount.tsx:1-45; Messages.module.css:266-302 | MISSING | Animate count changes (old/new numbers slide up or down). | S |
| Click toggles: add if the viewer has not reacted, else remove all the viewer's events in that group | src/features/messages/MessageReactions.tsx:71-89 | PARTIAL (crates/app/src/ui/emoji_picker.rs:325-356; message.rs:609-613) | The port removes only the first own event, and it matches groups by content only, not by content and emoji URL. Remove all own events and match on the custom emoji URL. | S |
| Toggling a custom-emoji pill reuses that pill's emoji tag (works even if the shortcode is not in the viewer's catalog) | src/features/messages/MessageReactions.tsx:84, 319 | PARTIAL (crates/app/src/ui/emoji_picker.rs:349) | The port sends only content, and the relay resolves it from the catalog (relay messages.rs:747-751). Pass the group's `CustomEmoji` to `react`. | S |
| Pills disabled when reacting is not allowed (read-only, archived, undelivered) | src/features/messages/MessageReactions.tsx:309-314; MessageRow.tsx:805-808 | MISSING | Pills are always clickable. Add the disabled state. | S |
| Reaction error alert "Could not update reaction. Try again." | src/features/messages/MessageReactions.tsx:90-95, 195, 438 | MISSING (crates/app/src/ui/emoji_picker.rs:351-353) | The port only logs. Show the error inline. | S |
| Failed/unknown reaction delivery: "Couldn’t update reaction." / "Reaction delivery not confirmed." + "Retry reaction" | src/features/conversation/ReactionTool.tsx:132-166; MessageReactions.tsx:439 | MISSING | Find kind-7/5 ops for the message in the outbox and show the status and retry. | S |
| Hover preview card: emoji glyph + emoji name (e.g. `:thumbsup:`, 12px tertiary caption), then names "You, Sam Rivera" (max-w 224) | src/features/messages/MessageReactions.tsx:288-333; reaction-name.ts:1-11 | PARTIAL (crates/app/src/ui/message.rs:587-591) | The port has a plain text tooltip ("👍 You, Sam"). Build the preview card with the glyph, the shortcode name lookup and a names line. | S |
| Reactor names order: "You" first, then reactors in event order; unknown names fall back to the first 10 hex chars; profiles fetched on hover | src/features/messages/MessageReactions.tsx:269-287 | DIFFERENT (crates/app/src/ui/message.rs:542-550) | The port sorts other names alphabetically and does not ask for missing profiles on hover. Keep event order and call `want_profiles` on hover. | S |
| Preview timing: 100ms open delay, instant switching along the row, slide ±12px with scale .98 over 160ms | src/features/messages/MessageReactions.tsx:44, 380-400; Messages.module.css:340-347 | MISSING | Default tooltip timing is used. Add the delay, the instant hand-off and the slide animation. | S |
| Inline add-reaction button after the pills (28px round, transparent until hover/open) | src/features/messages/MessageReactions.tsx:422-437 | PARTIAL (crates/app/src/ui/message.rs:616-663) | Works in the channel. In the thread panel it renders but does nothing (`picker: None`, no `on_popup`, thread.rs:446-449). Wire a picker there. | S |

### Quick reactions store

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Stored entries validated (`validReactionContent`, positive counts) | src/features/messages/quick-reactions.ts:11-24 | PARTIAL (crates/app/src/ui/emoji_picker.rs:237-245) | Port deserializes without validating content or counts. | S |
| Shortcuts frozen while in use; refreshed on reload or another window's update | src/features/messages/quick-reactions.ts:63-86 | DIFFERENT (crates/app/src/ui/channel.rs:709) | The port recomputes on every render, so the bar can reorder right after a reaction. Freeze it per page visit if parity matters. | S |
| History forgotten when leaving the community | src/features/messages/quick-reactions.ts:59-61 | PARTIAL (crates/relay/src/store.rs:397) | `purge` clears the partition's stored prefs, but the in-memory `Usage` global (emoji_picker.rs:226) keeps the entry until restart. Drop it on leave. | S |

### Emoji picker

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Category header labels stay sticky while scrolling | src/bundled/emoji/emoji-mart.ts:195-202 | MISSING (crates/app/src/ui/emoji_picker.rs:776-783) | Headers are plain list rows. Pin the current section's header at the top of the grid. | S |
| Nav icons: clock, smiley, paw, orange, barbell, car, lightbulb, shapes, flag, asterisk | src/bundled/emoji/emoji-mart.ts:16-28 | DIFFERENT (crates/app/src/ui/emoji_picker.rs:430-443) | The port uses Lemon for food and Category for symbols. Use the orange and shapes glyphs. | S |
| Responsive width: 1 to 6 per line from the composer/viewport width; the nav is hidden below 6 per line | src/bundled/emoji/EmojiPicker.tsx:103-139, 343-349; emoji-mart.ts:117 | PARTIAL (crates/app/src/ui/emoji_picker.rs:30-33, 854) | Fixed at 360px and 6 per line. Compute per-line from the available width and hide the nav when narrow. | S |
| Frequently used: emoji-mart history of every picker pick (composer and reactions), up to 2 rows; defaults +1, grinning, kissing_heart, heart_eyes, laughing, stuck_out_tongue_winking_eye (first row) | src/bundled/emoji/emoji-mart.ts:96-104, 115; node_modules/emoji-mart DEFAULTS | DIFFERENT (crates/app/src/ui/emoji_picker.rs:650-668) | The port shows one row from reaction usage only, so composer inserts never count. Record every pick, allow 2 rows (12), and keep the emoji-mart default order. | S |
| Skin tone persisted across opens/sessions (emoji-mart `skin` in localStorage); tooltip "Choose default skin tone" | src/bundled/emoji/emoji-mart.ts:364-368 (Mart storage) | PARTIAL (crates/app/src/ui/emoji_picker.rs:484, 933) | The tone lives only in the picker entity and is not saved. The tooltip says "Skin tone". Persist it as a device pref and match the copy. | S |
| Search ranking: Mart word-prefix results first, then fuzzy shortcode matches (exact, prefix, substring, subsequence); alias shortcodes | src/bundled/emoji/emoji-mart.ts:566-594; emoji-search.ts:48-67 | DIFFERENT (crates/app/src/ui/emoji_picker.rs:166-212, 684) | The port uses the composer's `searchEmoji` order (limit 240), not Mart's result order (limit 90). The results are close but not identical. | S |
| Search matches emoji keywords (e.g. "happy", "love") | src/bundled/emoji/emoji-search.ts:93; emoji-mart search index | MISSING (crates/app/src/ui/emoji_picker.rs:96-114) | The index holds only shortcodes and name words. Add the emoji-mart keyword data. | S |
| Search results header "Search Results" | @emoji-mart i18n `categories.search` | DIFFERENT (crates/app/src/ui/emoji_picker.rs:685) | The port says "Search results" (lower-case r). | S |
| Empty search state | Mart with preview off shows an empty result list | DIFFERENT (crates/app/src/ui/emoji_picker.rs:784-790) | The port shows "No emoji found". buzz-app shows nothing. Keep or drop, as you choose. | S |
| Keyboard navigation: arrow keys move a selection through results/grid from the search field; Enter picks the selection | src/bundled/emoji/emoji-mart.ts:474-496 (Mart owns arrows/Enter) | MISSING | There is no grid selection or arrow/Enter handling. Add a selected cell, arrow movement and Enter to pick. | M |
| Escape closes the picker and returns focus to its trigger | src/bundled/emoji/EmojiPicker.tsx:309-323 | DIFFERENT (crates/app/src/ui/emoji_picker.rs:850-853; channel.rs:150-159) | It closes, but the reaction picker sends focus to the composer input, not the trigger. | S |
| Reaction picker opens above the trigger (side top, 6px offset); composer picker anchored to the composer form (top, 4px) | src/bundled/emoji/EmojiPicker.tsx:328-337 | DIFFERENT (crates/app/src/ui/message.rs:447-455, 652-661) | The port opens the reaction picker below the bar or button. Anchor it above with a 6px gap, flipping on collision. | S |
| Custom emoji catalog error inside the picker + "Retry emoji" | src/bundled/emoji/EmojiPicker.tsx:271-278 | MISSING | Show the catalog error and a refresh button (`session.refresh_emoji`, relay session/mod.rs:385). | S |
| Search query carried between the Emoji and GIF tabs (both directions) | src/bundled/emoji/EmojiPicker.tsx:187-190, 236-239 | PARTIAL (crates/app/src/ui/emoji_picker.rs:612-628) | The emoji query goes to the GIF tab, but GIF edits do not come back to the emoji search. | S |

### GIF picker

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Tile hover brightens (filter brightness 1.08) | src/bundled/emoji/Emoji.module.css:104-106 | MISSING | Add a hover overlay or tint on GIF tiles. | S |

### Custom emoji (inline + settings)

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Copying a selection that contains custom emoji yields their `:code:` text | src/bundled/emoji/copy-emoji.ts:1-47; index.tsx:36-40 | PARTIAL (crates/app/src/ui/markdown.rs:390) | The emoji is an inline image whose alt is `:code:`, but the TextView copy path for inline elements is unverified. Confirm that copy writes the shortcode and add a test. | S |
| Tabs "Add emoji" / "My emojis (N)"; Add tab only when the session can author | src/bundled/emoji/CustomEmojiSettings.tsx:77-78, 353-368 | PARTIAL (crates/app/src/ui/settings/emoji.rs:475-499) | The Add tab is always shown. Hide it when upload/add are unavailable, and default to "My emojis". | S |
| Upload failure fallback copy "Failed to upload emoji image." | src/bundled/emoji/CustomEmojiSettings.tsx:146 | PARTIAL (crates/app/src/ui/settings/emoji.rs:134) | The port shows the raw upload error string. Use the fallback copy when the message is empty or generic. | S |
| Focus moves to Upload after Clear/Save/empty-state action | src/bundled/emoji/CustomEmojiSettings.tsx:88-107 | MISSING | Move focus to the Upload button after these actions. | S |

### Copy image, download, media modal boundary

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Media viewer modal: Escape closes, Tab trapped inside, focus restored to opener | src/features/messages/useModalBoundary.ts:11-113 | PARTIAL (crates/app/src/ui/media/review.rs:82-133, 229-241, 671-673) | Escape and focus restore are done. There is no Tab focus trap (the viewer occludes the background). | S |

## 3. Timeline, scrolling, unread, jump-to, threads, typing and search

Note: buzz-app draws no "new messages" unread line in the timeline; the only cue is the jump button's new-message count, which the port matches.

Counts: MISSING 43, PARTIAL 13, DIFFERENT 13, IN PROGRESS 0, DONE 61.

### Timeline layout, day dividers and grouping

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Day labels and bylines re-render when the local day rolls over at midnight (`useLocalDay`), so "Today" becomes "Yesterday" without new rows | src/shared/use-local-day.ts:76 | MISSING (crates/app/src/ui/message.rs:39) | Labels are computed once in `items()` and only rebuilt when rows change. Add a midnight timer that rebuilds items and notifies the channel page and thread panel. | S |
| First-load error: error text plus a "Retry messages" button that re-ensures the channel | src/bundled/channels/ChannelBody.tsx:81 | PARTIAL (crates/app/src/ui/channel.rs:716) | The port shows the error text only. Add a "Retry messages" button that retries the channel window. | S |
| Timeline end spacer (12px) is part of the last list item, so it scrolls with history | src/features/messages/ChannelTimeline.tsx:827 | DIFFERENT (crates/app/src/ui/channel.rs:764) | The port puts a fixed 12px div below the list, outside the scroll. Render the spacer as part of the last list row. | S |
| Narrow viewport (<= 650px): timeline padding-inline 12px instead of 24px | src/features/messages/Messages.module.css:504 | MISSING (crates/app/src/ui/channel.rs:752) | Use 12px row padding when the window is <= 650px wide. | S |

### Membership rows (as they affect the timeline)

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Membership row gets its own day divider and is excluded from read observation | src/features/messages/MembershipRow.tsx:40 / src/features/messages/use-reading.ts:92 | PARTIAL (crates/app/src/ui/channel.rs:172) | Day divider is done. The port reports membership row ids to `viewport()`; filter them out as buzz-app does. | S |

### Scrolling, older history and scroll restore

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Per-channel scroll restore: saves offset, at-bottom flag and an anchor message plus its Y offset in view state (`scroll:<channel>`), restores it on return, survives restarts | src/features/messages/ChannelTimeline.tsx:185 / src/features/messages/ChannelTimeline.tsx:430 / src/features/messages/ChannelTimeline.tsx:513 | MISSING (crates/app/src/ui/workspace.rs:240) | A new `ChannelPage` is built on every channel switch and starts at the bottom. Save the anchor row id + offset (and bottom flag) per scope/channel when the page closes, persist it, and scroll to it on reopen. Keep the anchor through late measurements and membership regrouping. | M |
| Row height cache per channel (`geometry`) so a restored position paints without jumps | src/features/messages/ChannelTimeline.tsx:396 | MISSING (crates/app/src/ui/channel.rs:88) | Cache measured row heights per channel/width and seed the `ListState` on reopen (needed for exact restore). | M |
| Retained older-history demand while the window is cached, resumed when it verifies | src/features/messages/ChannelTimeline.tsx:640 | MISSING (crates/app/src/ui/channel.rs:179) | When a load is skipped because the window is stale/cached, remember the demand and call `load_older` again once the window is fresh. | S |
| Top edge shows the window error text (role alert) next to the retry button | src/features/messages/ChannelTimeline.tsx:739 | MISSING (crates/app/src/ui/channel.rs:537) | Render `snapshot.error` text in the edge row when rows exist. | S |
| Keyboard scrolling of the focusable history region (Up, PageUp, Home, Shift+Space, Ctrl+Home leave follow; region labelled "Channel message history") | src/features/messages/ChannelTimeline.tsx:37 / src/features/messages/ChannelTimeline.tsx:720 | MISSING (crates/app/src/ui/channel.rs:741) | Make the timeline focusable and handle arrow/page/home/end keys to scroll the list; upward keys must pause tail follow. | S |
| Resize and reflow keep the reading anchor (or bottom) through late row measurements | src/features/messages/ChannelTimeline.tsx:560 | PARTIAL (crates/app/src/ui/channel.rs:88) | GPUI `ListState` keeps the item offset on reflow; there is no explicit anchor-row retention for width changes while scrolled up. Verify and keep the top visible row fixed on width change. | S |

### Jump to latest

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Show/hide motion: opacity, translateY(2px) scale(.97), blur(2px) over 150ms in / 120ms out; opacity only with reduced motion | src/features/messages/Messages.module.css:2276 | MISSING (crates/app/src/ui/channel.rs:765) | The port mounts/unmounts the pill instantly. Animate opacity/translate/scale on show and hide. | S |
| Button moves up 48px into a separate lane when the composer shows an agent-activity pill | src/features/messages/Messages.module.css:2271 | MISSING (crates/app/src/ui/channel.rs:597) | Depends on the agent activity chooser (see Typing). Offset the pill when it is present. | S |
| Thread panel has the same jump-to-latest button with new-reply count; smooth scroll with a 1s settle | src/features/messages/ThreadPanel.tsx:754 / src/features/messages/ThreadPanel.tsx:1001 | MISSING (crates/app/src/ui/thread.rs:454) | Add the pill to the thread panel: track distance from bottom, count replies that arrive while scrolled up, click resumes tail follow. | S |

### Jump to a message (reveal)

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Revealed row is focused (keyboard focus moves to the row; no colour flash) | src/features/messages/use-message-reveal.ts:74 | DIFFERENT (crates/app/src/ui/navigation.rs:33) | The port tints the row with `affordance_accent` for 2s instead of focusing it. buzz-app has no highlight flash. Decide: drop the tint and move focus to the row, or keep it as a deliberate extra. | S |
| Exact target missing: "Selected message unavailable." plus "Retry thread" | src/features/messages/ThreadPanel.tsx:1060 | MISSING (crates/app/src/ui/thread.rs:215) | When `target_status` is Unavailable, show the status line and a Retry thread button. | S |
| Navigation that fails shows "This destination couldn’t open" with "Retry navigation" | src/app/App.tsx:255 | MISSING (crates/app/src/ui/workspace.rs:772) | `Reveal::Absent` falls through to an exact thread silently. Show a failure notice when the exact lookup reports not-found/unavailable. | S |

### Unread and mark-as-read

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Only fully visible rows count (partly clipped rows are not read) | src/features/messages/use-reading.ts:94 | PARTIAL (crates/app/src/ui/channel.rs:170) | The port sends the whole `visible_range`, including partly visible rows. Trim rows whose bounds are clipped by the viewport. | S |
| Reading only while the window is visible and focused and focus is in the list or its composer; blur/hidden stops the lease | src/features/messages/use-reading.ts:67 / src/features/messages/use-reading.ts:226 | MISSING (crates/app/src/ui/channel.rs:178) | `ChannelView::viewport` documents "report nothing while hidden or unfocused" but the page never does. Observe window activation and focus and report `([], false)` when inactive; re-report on focus. | S |
| Reading re-armed by scroll, pointer, key, resize, DOM changes and owner positioning | src/features/messages/use-reading.ts:219 | PARTIAL (crates/app/src/ui/channel.rs:163) | The port reports on scroll events only; arrivals at the bottom are handled in the relay. Also report after resize and after programmatic positioning (reveal, jump). | S |
| Thread panel reading: visible replies are observed and the thread is caught up to its newest reply (`catchUp(bottom, rootId)`) | src/features/messages/ThreadPanel.tsx:926 | MISSING (crates/app/src/ui/thread.rs:133) | Add a thread viewport/reading API in the session (lease scoped to channel + root) and report visible reply ids and bottom state from the thread list. | M |

### Thread summary in the timeline

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Unread dot on the summary when the thread has unread replies or is marked unread (tooltip "Thread marked unread" / "...on this device only" / "Observed unread replies…") | src/features/messages/ReplySummary.tsx:67 / src/features/messages/MessageRow.tsx:209 | MISSING (crates/app/src/ui/message.rs:986) | Expose per-thread unread state from the session (`ReadTarget` thread snapshot) and draw the 8px `text-standard` dot after the count with the tooltip text. | M |
| Working dots (three pulsing dots) on the summary while one of the viewer's agents is active in that thread | src/features/messages/MessageRow.tsx:877 / src/features/messages/Messages.module.css:626 | MISSING (crates/app/src/ui/message.rs:949) | Match `thread_typing` entries for the root against the viewer's own agents and show the animated dots pill. | S |
| "View thread" button on a root with no replies while an agent works in its thread | src/features/messages/MessageRow.tsx:891 | MISSING (crates/app/src/ui/message.rs:312) | Show the summary (dots + "View thread") when `reply_count == 0` and an owned agent is typing in that thread. | S |

### Thread panel: shell, header and layout

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Close button takes focus when the panel opens (standalone header) | src/features/messages/ThreadPanel.tsx:129 | DIFFERENT (crates/app/src/ui/workspace.rs:292) | The port focuses the thread composer on open. buzz-app focuses the close button (or the composer for a "reply" intent). Match per intent. | S |
| Resizable dock: 16px handle on the border, width clamped [316px, available − 320px], double-click resets | docs/port/ui.md (4.11) | MISSING (crates/app/src/ui/thread.rs:556) | Add a drag handle that sets the dock width, clamps it and resets on double-click; persist the width. | M |
| Narrow layouts: <= 1000px viewport or <= 651px container overlays the thread on the right (min(420px,100%)), <= 650px full width | docs/port/ui.md (4.11) | MISSING (crates/app/src/ui/thread.rs:556) | Switch the board to an overlay layout under the breakpoints. | M |
| Open/close motion: translateX(12px) + opacity over 180ms | docs/port/ui.md (4.11) | MISSING (crates/app/src/ui/thread.rs:556) | Animate the dock in and out. | S |

### Thread panel: content and reply tree

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Narrow panels flatten indentation at depth 3 (<= 360px) or 4 (<= 480px) | docs/port/ui.md (4.11) | MISSING (crates/app/src/ui/thread.rs:31) | Make `MAX_INDENT` depend on the panel width. | S |
| Branch summary shows "(N new)" when descendants are unread | src/features/messages/ReplySummary.tsx:64 / src/features/messages/ThreadPanel.tsx:802 | MISSING (crates/app/src/ui/thread.rs:938) | Needs per-reply unread attention from the session; append "(N new)". | S |
| Branch summary avatars use agent squircles and media-proxied pictures | src/features/messages/ReplySummary.tsx:38 | PARTIAL (crates/app/src/ui/thread.rs:926) | The port uses the raw picture URL and never squircles agents here. Use `media::avatar` and the agent shape as in `thread_summary`. | S |
| Branch summary is a link-variant small button | src/features/messages/ReplyBranch.tsx:80 | DIFFERENT (crates/app/src/ui/thread.rs:950) | The port styles it like the timeline thread pill (hover fill, 32px). Use link-variant styling. | S |
| Expanding a branch keeps the reading position, moves focus to the first revealed reply and scrolls it into view (nearest) | src/features/messages/ReplyBranch.tsx:36 | PARTIAL (crates/app/src/ui/thread.rs:242) | The port pauses tail follow only. Scroll the first child into view after expanding. | S |
| Older replies load when the reader scrolls near the top (strict "older" windows only), keeping the anchor row in place and expanding newly loaded ancestors | src/features/messages/ThreadPanel.tsx:898 / src/features/messages/ThreadPanel.tsx:451 | PARTIAL (crates/app/src/ui/thread.rs:133) | Paging and position keeping are done. Ancestors that arrive with an older page are not auto-expanded around the anchored reply. | S |

### Thread panel: states

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Older-page error shown inline at the top of the reply list with its own "Retry thread" (re-pages older) | src/features/messages/ThreadPanel.tsx:1038 | DIFFERENT (crates/app/src/ui/thread.rs:479) | The port shows every error below the list. Put the older-page error at the top of the replies. | S |
| Loading older replies indicator | src/features/messages/ThreadPanel.tsx:914 | DIFFERENT (crates/app/src/ui/thread.rs:524) | The port adds "Loading earlier replies…", which buzz-app does not show. Remove it or keep as an intended extra. | S |
| Saved-draft guard: "This saved draft does not identify a thread root. Open its origin to check the destination." and composer disabled until the root verifies | src/features/messages/ThreadPanel.tsx:1049 | MISSING (crates/app/src/ui/thread.rs:538) | Only relevant if drafts can open threads; add the guard to that entry point. | S |

### Thread panel: composer and actions

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Reply to a specific reply: "Replying to {name}" + excerpt + cancel X; clicking Reply on the same reply again clears the target | src/features/messages/ThreadPanel.tsx:521 / src/features/messages/ThreadPanel.tsx:1110 | PARTIAL (crates/app/src/ui/thread.rs:249) | Context bar is done (crates/app/src/ui/composer.rs:1665). The second Reply click does not toggle the target off. | S |
| Reply on the root (action bar) focuses the composer and clears any reply target | src/features/messages/ThreadPanel.tsx:517 | MISSING (crates/app/src/ui/thread.rs:803) | The root has no reply control in the port. Add it (via the action bar). | S |
| After sending a nested reply, its branch and ancestors open and the new row scrolls into view; a root-level reply follows the bottom | src/features/messages/ThreadPanel.tsx:1131 / src/features/messages/ThreadPanel.tsx:742 | PARTIAL (crates/app/src/ui/thread.rs:284) | Branch expansion is done; the new row is not scrolled into view once it lands. | S |

### Typing indicator

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Narrow (<= 650px) inset 12px | src/features/messages/TypingIndicator.module.css:108 | MISSING (crates/app/src/ui/typing.rs:96) | Use 12px insets below 650px. | S |
| Typing indicator in the thread panel, scoped to that thread root | src/features/messages/TypingIndicator.tsx:88 | MISSING (crates/app/src/ui/thread.rs:538) | Data exists (`thread_typing`, crates/relay/src/session/mod.rs:112). Render `typing::overlay` above the thread composer. | S |
| Names use channel-scoped identity names (nicknames) | src/features/messages/TypingIndicator.tsx:94 | PARTIAL (crates/app/src/ui/typing.rs:64) | The port uses the profile name. Use the same resolver as message bylines once identity names exist. | S |
| Viewer's own working agents split out of "typing" into an agent chooser on the right: up to 4 overlapping 24px squircle avatars, "+N" | src/features/messages/TypingIndicator.tsx:188 | MISSING (crates/app/src/ui/typing.rs:59) | The port lists every pulse author as "typing". Split owned agents (agent choices) from others and render the avatar stack as a ghost xs button. | M |
| Agent chooser hover (250ms) opens a "Working now" popover listing each agent row with status, scope ("Channel conversation", "Thread · {excerpt}", "+N more") and open actions | src/features/messages/TypingIndicator.tsx:137 / src/features/messages/TypingIndicator.tsx:353 | MISSING (crates/app/src/ui/typing.rs:59) | Build the popover with `AgentActivityRow` equivalents and conversation/activity navigation. | M |
| Single-agent click opens its activity panel directly (Down Arrow previews the list); Escape closes the popover | src/features/messages/TypingIndicator.tsx:280 | MISSING (crates/app/src/ui/typing.rs:59) | Add click-to-open and keyboard handling once the chooser exists. | S |

### Search palette (Search Buzz / Search this conversation)

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Ctrl/Cmd+F opens "Search this conversation" scoped to the open conversation | src/app/shell/PageSearch.tsx:106 | MISSING (crates/app/src/ui/workspace.rs:541) | Add the binding; open the dialog with `scope` preset to the current channel. | S |
| Shortcut hint follows the user's rebinding | src/app/shell/PageSearch.tsx:150 | MISSING (crates/app/src/ui/workspace.rs:394) | Tooltip is hard-coded "Ctrl+K". Derive from the shortcut bindings when rebinding exists. | S |
| No community / not connected: pages-only palette; "Choose a community to see recent conversations.", "Connecting to this community…", "Connect to this community to search messages.", disconnected notice with "Retry connection" | src/app/shell/PageSearch.tsx:258 | MISSING (crates/app/src/ui/workspace/dialogs.rs:68) | `open_search` returns when there is no community and never shows connection states. Allow a pages-only palette and the connection notices. | S |
| Recent activity rows show the channel's last-message preview as detail | src/app/shell/SearchResults.tsx:532 | DIFFERENT (crates/app/src/ui/search.rs:565) | The port shows the conversation type. Use `channel.preview` when present. | S |
| "Loading recent conversations…" while the channel list loads | src/app/shell/SearchResults.tsx:629 | MISSING (crates/app/src/ui/search.rs:567) | Show it while the roster is loading. | S |
| Matched letters underlined in result labels (`MatchedLabel`, also used by other pickers) | src/features/search/MatchedLabel.tsx:24 / src/features/search/match.ts:62 | MISSING (crates/relay/src/search.rs:127) | `match_name` returns only a rank. Return matched code-point positions and render underlined runs in `modal::row`. | S |
| Usage-ranked results: visits and past choices for the same typed text (7-day half-life, max 1.5 rank boost), stored per community/viewer | src/features/search/usage.ts:100 / src/features/search/usage.ts:161 | MISSING (crates/app/src/ui/search.rs:599) | Port the usage store (visits recorded on navigation, picks on choice) and apply `searchOrder` to channels and pages. | M |
| Archived writable channels (stream/forum/session) included with detail "Archived channel", ranked half a step behind live ones | src/app/shell/SearchResults.tsx:87 / src/app/shell/SearchResults.tsx:475 | DIFFERENT (crates/app/src/ui/search.rs:417) | The port filters out all archived channels. Include writable archived ones with the +0.5 rank. | S |
| Message hit detail "{conversation} · {author} · {M/D/YYYY}" with workflow label for workflow output | src/app/shell/SearchResults.tsx:562 | PARTIAL (crates/app/src/ui/search.rs:476) | Add the workflow label when the hit has a workflow owner. | S |
| `in:` that names nothing: buzz-app shows "Searching messages…" during lookup then the normal empty text | src/app/shell/SearchResults.tsx:567 | DIFFERENT (crates/app/src/ui/search.rs:493) | The port shows "No conversation matches that in: name.". Use buzz-app's wording or keep as intended. | S |
| Author candidates include the viewer's selectable agents (matched by profile name or agent name) and hide archived identities (NIP-IA) | src/app/shell/SearchResults.tsx:304 / src/features/messages/use-mention-archives.ts:7 | MISSING (crates/app/src/ui/search.rs:333) | The port uses scope members and relay people only. Add agent choices and an identity-archive filter. | M |
| Author picker avatars use squircles for agents | src/app/shell/SearchResults.tsx:367 | MISSING (crates/app/src/ui/search.rs:843) | Squircle the avatar when the candidate is an agent. | S |
| Ambiguous completed author name reopens the People/Agents picker so the user can pick one key | src/app/shell/SearchResults.tsx:234 | DIFFERENT (crates/app/src/ui/search.rs:495) | The port shows "Several people have that name. Type from: and pick one." Show the picker instead. | S |
| Message search error detail "Message search couldn’t finish: … Pages and conversations are still available." with "Retry messages" | src/app/shell/useSearchMessages.ts:247 / src/app/shell/SearchResults.tsx:785 | MISSING (crates/app/src/ui/search.rs:491) | Show the error detail and a retry button that re-runs the search. | S |
| Status notes: "Loading joined conversations…", "Couldn’t load all joined conversations." + "Retry conversations", "Conversation names include only loaded joined conversations.", "Public channel results include only the first page of channels.", public lookup error + "Retry channels" | src/app/shell/SearchResults.tsx:728 | MISSING (crates/app/src/ui/search.rs:817) | Public channel errors are swallowed (`unwrap_or_default`). Add the status area with these notes and retries. | S |
| Highlight clamps at the ends (no wrap) | src/features/search/use-search-highlight.ts:67 | DIFFERENT (crates/app/src/ui/search.rs:660) | The port wraps with `rem_euclid`. Clamp instead. | S |
| No highlight before anything is typed (Enter does nothing on an empty query); typing highlights the first row | src/features/search/use-search-highlight.ts:54 | DIFFERENT (crates/app/src/ui/search.rs:172) | The port always highlights row 0. Highlight nothing while the query is empty. | S |
| Highlight follows the row's key as late results arrive (a row inserted above cannot redirect Enter); falls back to the first row when the highlighted row leaves | src/features/search/use-search-highlight.ts:173 | DIFFERENT (crates/app/src/ui/search.rs:110) | The port stores an index. Store the highlighted entry key. | S |
| Keyboard moves scroll the highlighted row into view | src/features/search/use-search-highlight.ts:96 | MISSING (crates/app/src/ui/search.rs:964) | Track the results scroll handle and scroll the highlighted row into view on each move. | S |
| Pointer moves highlight a row (real moves only, not rows moving under a resting cursor) | src/features/search/use-search-highlight.ts:156 | PARTIAL (crates/app/src/ui/search.rs:863) | Hover highlighting works, but it also fires when results move under a still pointer. Ignore mouse-move events with unchanged position. | S |
| Shift+Ctrl/Cmd+1…9 runs the Nth result directly | src/app/shell/SearchChoices.tsx:122 | MISSING (crates/app/src/ui/search.rs:938) | Handle shift+secondary+digit in the key handler. | S |
| Field limits: max 256 characters, no spellcheck/autocorrect | src/app/shell/SearchChoices.tsx:117 | MISSING (crates/app/src/ui/search.rs:132) | Cap the input at 256 characters. | S |
| Results for a conversation the viewer leaves are dropped while the palette is open | src/app/shell/useSearchMessages.ts:79 | MISSING (crates/app/src/ui/search.rs:461) | Message hits are looked up against `snapshot.channels` and `outcome.channels`, so stale hits stay. Filter hits to channels still listed. | S |

## 4. Composer, autocomplete, formatting, attachments and voice recording

Note: buzz-app's composer is a WYSIWYG ProseMirror editor (marks and blocks are document nodes, serialized to Markdown on send). The port's composer is a plain-text `TextareaState` with atomic mention/channel chips, and its toolbar inserts Markdown syntax. Rows that depend on the live-styling work are marked IN PROGRESS.

Note: buzz-app does not turn a URL pasted over a selection into a link; it replaces the selection (`src/features/messages/EditableInput.tsx:1447-1492`).

Counts: MISSING 42, PARTIAL 20, DIFFERENT 11, IN PROGRESS 4, DONE 42.

### Input basics, keyboard and states

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Holding Enter (key repeat) never sends | src/features/messages/MessageComposer.tsx:1572 | MISSING (crates/app/src/ui/composer.rs:224) | Ignore repeated Enter (`is_held`) for submit. | S |
| Enter inside a bullet/ordered list continues the list instead of sending; Enter on an empty item exits the list | src/features/messages/MessageComposer.tsx:1573-1579, src/features/messages/composer-blocks.ts:513 | MISSING | Detect a list line at the caret: Enter inserts the next marker (`- `, `n+1. `), and an empty item removes its marker. Shift+Enter continues the block. Coordinate with the live-styling work. | M |
| Tab / Shift+Tab indent and outdent list items (sink/lift) | src/features/messages/EditableInput.tsx:1327-1340 | MISSING (crates/app/src/ui/composer.rs:2637 only handles completion) | Without a completion, Tab on a list line nests the item (indent plus renumber) and Shift+Tab lifts it. | M |
| Typed Markdown conversion is its own undo step (one undo restores the typed delimiter) | src/features/messages/EditableInput.tsx:1262-1270 | IN PROGRESS | Being added with the live-styling work. | M |
| 16,000-character limit enforced by the editor (an insertion that is too long is refused: "Message is too long to insert text") | src/features/messages/MessageComposer.tsx:806-808, 1507 | DIFFERENT (crates/app/src/ui/composer.rs:616-621, 406-410) | The port lets the text grow past the limit, shows "Messages hold up to 16000 characters." and disables Send. buzz-app refuses the input. Decide which to keep; the reference behaviour is to cap input. | S |
| Emoji-only draft shows large emoji in the composer | src/features/messages/MessageComposer.tsx:567-570, 1506 | MISSING | When the draft is Unicode or custom emoji only, render the textarea text at the large emoji size. | S |
| Archived channel: the draft is kept, but the composer is disabled until the channel is restored | src/features/messages/MessageComposer.tsx:313-322, 495-498 | MISSING (crates/app/src/ui/channel.rs:579-584 checks only `read_only`) | Disable input and Send for archived channels and keep the draft visible. | S |
| Cached channel (offline, not yet confirmed): composer shows the local draft, disabled, with no completion or typing | src/features/messages/MessageComposer.tsx:320-322, 1264-1276 | DIFFERENT (crates/app/src/ui/channel.rs:580-582, crates/relay/src/discovery.rs:1575) | A cached summary is `read_only`, so the port shows the "Read-only preview · You haven't joined" join notice. Show the disabled composer with its draft instead. | S |
| Relay connection that cannot write: "This relay connection supports reading only." | src/features/messages/MessageComposer.tsx:1329-1337 | MISSING | Show this footer when the outbox cannot publish kind 9. | S |
| Composer disabled while a send, admission or edit save is in flight | src/features/messages/MessageComposer.tsx:499-506, 1478 | PARTIAL (crates/app/src/ui/composer.rs:84, 406-412) | `sending` blocks Send only. The text stays editable while the send is in flight. Make the input read-only while busy. | S |
| New-conversation composer (New message) is the full MessageComposer in submission mode, with a draft roster | src/features/messages/MessageComposer.tsx:216-227, 245-260, src/features/messages/draft-mention-roster.ts:5 | PARTIAL (crates/app/src/ui/new_message.rs:51) | The port uses a bare textarea: no formatting, completions, emoji picker or mention chips. Reuse `Composer` with a submission/roster mode. | M |
| Composer lab / state gallery (developer specimens: unavailable, read-only, validation) | src/bundled/composer/lab/ComposerStateGallery.tsx:53-78, src/bundled/composer/lab/ComposerCopyPreview.tsx:6-28 | MISSING | Developer-only gallery; optional. | S |

### Drafts and send

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Draft conflict between two composers/windows: "This draft changed elsewhere..." with Load saved draft / Keep my draft | src/features/messages/MessageComposer.tsx:594-623, 661-671, 1667-1697 | MISSING | Compare against the stored revision before writing. On a mismatch, show the conflict notice and its two buttons. | M |
| Draft storage failure: "Could not save this draft on this device..." + Retry draft save | src/features/messages/MessageComposer.tsx:459-467, 1676, 1691-1695 | MISSING (crates/app/src/ui/composer.rs:695-697 only logs) | Surface the save error in the composer with a retry button. | S |
| Accepted-send cleanup failure: "Message accepted. Could not update the saved draft..." + Retry draft cleanup | src/features/messages/MessageComposer.tsx:672-696, 1672-1681 | MISSING | Follows from the draft-storage rows. | S |
| Remember mentioned agents: after a send, the next draft is prefilled with the agent chips (setting in Agent settings) | src/features/messages/MessageComposer.tsx:1124-1133, src/features/messages/mention-preferences.ts:7-43 | MISSING | Build a follow-up draft of the agent recipients, and add the "remember mentioned agents" preference. | S |
| A selected recipient is archived: "A selected recipient is archived. Remove it before sending." | src/features/messages/MessageComposer.tsx:1046-1049 | MISSING | Check archive state at send. | S |
| Session / personal ("Me") composer: mentioned agents are added to the session (and parent channel) on send; "Retry agents" | src/features/messages/MessageComposer.tsx:245-260, 942-982, 1069-1073 | DIFFERENT (crates/app/src/ui/composer.rs:184-191) | The port turns session outsiders into references. buzz-app adds them as session members before sending. This may belong to the Sessions audit. | M |
| "Retry message preparation" when the custom emoji catalog failed | src/features/messages/MessageComposer.tsx:1710-1728 | MISSING | Offer `refresh_emoji` next to a send error while the catalog is in error. | S |

### Formatting tools and link dialog

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Button tooltips show the platform shortcut ("Bold (⌘B)" on macOS, "Ctrl+B" elsewhere) | src/features/messages/ComposerFormattingTools.tsx:166-183 | PARTIAL (crates/app/src/ui/composer.rs:2797-2810) | The tooltips are hard-coded "Ctrl+…". Use ⌘ on macOS, and label Link "Link (⌘K)". | S |
| Active (pressed) state reflects the formatting at the caret | src/features/messages/ComposerFormattingTools.tsx:165, 178-184 | PARTIAL (crates/app/src/ui/composer.rs:2883-2903) | Active only when the delimiters touch the selection exactly. A caret inside `**bold**` does not light Bold. Follow the live-styling parse. | S |
| WYSIWYG formatting: toggles apply real marks/blocks (no visible delimiters), serialized to Markdown on send | src/features/messages/composer-markdown.ts:198-201, src/features/messages/composer-blocks.ts:147 | IN PROGRESS (crates/app/src/ui/composer.rs:2905-3019 inserts Markdown syntax) | Live styling work. | L |
| Collapsed-caret toggle: inline formats start a typing mode; Spoiler with a collapsed caret toggles the whole draft | src/features/messages/MessageComposer.test.tsx:3804 | DIFFERENT (crates/app/src/ui/composer.rs:3010-3016) | The port inserts an empty delimiter pair around the caret. | S |
| Toolbar open/close animation (140 ms fade/scale; leading tools slide in), instant for keyboard toggles and reduced motion | src/features/messages/ComposerFormattingTools.tsx:35, 96-97, 114-221 | MISSING | Add the reveal transition. | S |
| Link button / Mod+K opens the "Add link" / "Edit link" dialog with Text and URL fields | src/features/messages/ComposerLinkDialog.tsx:53-113, src/features/messages/EditableInput.tsx:1301-1310 | DIFFERENT (crates/app/src/ui/composer.rs:2972-2987) | The port inserts `[text](https://)` and selects the URL, and has no Mod+K. Build the dialog: initial focus is URL when text exists, else Text; Save is disabled when the URL is empty. | M |
| Link URL validation: "Use an HTTP(S) URL or a valid buzz:// link." (credentialed URLs refused); "Link text must stay on one line." | src/features/messages/ComposerLinkDialog.tsx:30-38, src/features/messages/composer-link.ts:5-8 | MISSING | Validate with the same safe-URL rule as message links. | S |
| "Remove link" in the dialog for an existing link; Mod+K on a detected plain URL edits it | src/features/messages/ComposerLinkDialog.tsx:58-67, src/features/messages/EditableInput.test.tsx:864 | MISSING | Find the link at the caret and offer Remove. | S |
| Typed Markdown spans convert as you type (`**b**`, `*i*`, `` `c` ``, `~~s~~`) using the timeline's parser rules | src/features/messages/composer-inline-input.ts:110 | IN PROGRESS | Live styling. | M |
| Typing ` ``` ` / `~~~` opens a code block; `- `, `1. `, `> ` open lists/quotes (nesting rules) | src/features/messages/composer-blocks.ts:329, 404 | IN PROGRESS | Live styling. | M |
| Backspace at the start of a code block leaves it | src/features/messages/EditableInput.tsx:1352-1361 | MISSING | Part of block editing; follows the live-styling model. | S |

### Paste, copy and drag-and-drop

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Pasting the app's own HTML restores its Markdown (identity links, formatting); other HTML pastes as plain text | src/features/messages/EditableInput.tsx:1456-1466, src/features/messages/composer-clipboard.ts:165 | PARTIAL (crates/app/src/ui/composer.rs:1318) | Plain text is used everywhere. Read Buzz's HTML flavour back as Markdown. | M |
| Pasted identity links (`[@Name](nostr:npub…)`) become recipient chips when admitted (at most 32, at a mention boundary); otherwise they stay display chips | src/features/messages/EditableInput.tsx:899-938, src/features/messages/mention-admission.ts:58-82 | MISSING | After a paste, scan for profile links and convert the admitted ones to chips. | M |
| A pasted lone URL gets a trailing space | src/features/messages/EditableInput.tsx:1467-1488 | MISSING | Append a space after a paste that is exactly one link. | S |
| Copy/cut from the composer writes humanized text and semantic HTML (chips read as `@Name` / `#channel`) | src/features/messages/EditableInput.tsx:885-897, 1437-1446, src/features/messages/composer-clipboard.ts:112-160 | PARTIAL (gpui-kit textarea copy) | Plain text only, and channel chips copy as raw Markdown link source. Copy chip labels and add the HTML flavour. | S |
| Pasting an agent snapshot (Buzz HTML) attaches the uploaded snapshot | src/features/messages/MessageComposer.tsx:1361-1387 | MISSING | Recognise the snapshot clipboard payload and add it as a ready attachment. | M |
| The whole conversation pane is the drop zone (`data-attachment-drop-zone`), routed to that pane's composer only | src/features/messages/use-file-drop.ts:17-82 | PARTIAL (crates/app/src/ui/composer.rs:2665-2677) | Only the composer box accepts drops. Extend the drop target to the channel and thread panes. | S |

### Attachments and uploads

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Uploads deferred until Send: files stay "Queued" and upload only on Send, then publish in the background | src/features/messages/attachment-draft.ts:257-266, src/features/messages/background-upload.ts:108-196 | DIFFERENT (crates/app/src/ui/composer.rs:1098-1141, 406-412) | The port uploads each file as soon as it is attached and blocks Send until all are ready. buzz-app clears the composer at once and uploads behind it. | L |
| Background upload pill: "Preparing" / "Uploading" / "Finishing" with a progress bar (indeterminate sweep without totals) and Cancel | src/features/messages/BackgroundUploadStatus.tsx:37-89 | MISSING | Needs the background-send model. Cancel stops the newest send and restores its draft. | M |
| Failed background send restores the draft and files, with the toast "Upload failed: …" / "Message failed to send: …" and recovery notices | src/features/messages/background-upload.ts:141-189, src/features/messages/BackgroundUploadStatus.tsx:90-104 | MISSING | Follows the background-send model. | M |
| A conversation cannot send again until its background send settles | src/features/messages/background-upload.ts:208-212 | MISSING | Follows the background-send model. | S |
| Per-file byte progress during transfer | src/features/messages/attachment-draft.ts:129-140 | MISSING (crates/relay/src/upload.rs:286-363) | Report the PUT body progress and show it. | M |
| Chip statuses: Queued / Preparing… / Uploading… / Ready / Upload failed | src/features/messages/ComposerAttachments.tsx:119-125 | PARTIAL (crates/app/src/ui/composer.rs:1548-1557) | No "Preparing…" state (metadata cleanup happens inside upload). | S |
| Video chips show a first-frame thumbnail | src/features/messages/ComposerAttachments.tsx:94-96, 199-213 | MISSING (crates/app/src/ui/composer.rs:1393-1402 images only) | Decode a poster frame for local video files. | M |
| Clicking a thumbnail opens a full preview dialog (image or playable video) | src/features/messages/ComposerAttachments.tsx:185-190, 285-310 | MISSING | Open the media viewer for a local draft attachment. | S |
| Size format: "N KB" (minimum 1 KB) under 1 MiB, else "N.N MB" | src/features/messages/ComposerAttachments.tsx:114-118 | DIFFERENT (crates/app/src/ui/composer.rs:1405-1422) | The port shows "B" units and other rounding. Match the reference. | S |
| Poof animation and "plop" sound on removing an attachment | src/features/messages/AttachmentPoof.tsx:11-106, src/features/messages/ComposerAttachments.tsx:52-55 | MISSING | Five-frame poof sprite at the chip plus a 0.34-volume sound. | S |
| Empty or oversized file is refused at add, with the "Could not attach file" toast | src/features/messages/attachment-draft.ts:193-195, src/features/messages/MessageComposer.tsx:1442-1448 | DIFFERENT (crates/app/src/ui/composer.rs:1062-1064, 2690-2696) | The port adds a failed chip and shows inline warning text instead of a toast. | S |
| Draft attachment byte cap: "Attachment drafts are full (1,000 MiB)…" | src/features/messages/attachment-draft.ts:196-205 | MISSING | Track the retained bytes across drafts. | S |
| Draft attachments survive navigation (tab-local per channel/thread) and pause or resume uploads ("Upload paused. Retry to continue.") | src/features/messages/attachment-draft.ts:37-49, 71-85 | MISSING (crates/app/src/ui/workspace.rs:240-241 recreates the page and composer) | Keep the attachment drafts in a session-level store keyed like the text draft. | M |
| Attaching while editing / unavailable: "Finish editing before attaching new files." / "Uploads are unavailable on this connection." | src/features/messages/MessageComposer.tsx:540-551 | PARTIAL (crates/app/src/ui/composer.rs:1049-1051, 1909-1916) | The port hides Attach during edits and drops the files quietly. Add the messages. | S |
| PNG/WebP with an ICC profile or EXIF orientation are re-rendered (orientation and colour applied), with WebP re-encoded losslessly | src/features/messages/image-metadata.ts:85-91, 136-140, src/features/messages/encode-webp.ts:2-41 | PARTIAL (crates/relay/src/upload.rs:149-180, 242-280) | The port drops the ICC/EXIF chunks without applying them, so rotated or wide-gamut images can come out wrong. | M |
| HEIC → JPEG and video → MP4 conversion before upload (host ffmpeg) | src/features/relay/attachments.ts:108-170 | MISSING (crates/relay/src/upload.rs:6-8) | Add a native transcoder (GStreamer is already used for voice). | L |

### Mentions, teams and outside-channel prompt

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Multi-word query admitted only while it starts a known name; a complete name plus a space closes it | src/bundled/mentions/mention-query.ts:22-31 | PARTIAL (crates/app/src/ui/composer.rs:786-789) | Only checks for a tier ≤ 1 match among loaded profiles; there are no aliases or directory names. | S |
| Matching tiers 0-3 (equal, prefix, word equals, word prefix) after NFKD/diacritic folding; substring never matches; aliases add 4 | src/bundled/mentions/README.md:103-146, src/bundled/mentions/mention-ranking.ts:19-34 | DIFFERENT (crates/app/src/ui/composer.rs:1425-1445) | The port uses a substring tier (`oney` finds Honey), lowercase only, and no word-equals tier or aliases. Implement the spec and run the fixtures (`mention-rules.fixtures.json`). | M |
| Order: members first, match, base match, owned agents, agent blocks, recency, managed, presence, label, key | src/bundled/mentions/README.md:148-174, src/bundled/mentions/mention-ranking.ts:35-92 | PARTIAL (crates/app/src/ui/composer.rs:790-798) | Has members → tier → name only. Add the remaining keys. | M |
| Choice set: members, offered agents, and a community directory search (200 ms debounce) for outsiders; archived identities excluded | src/bundled/mentions/useMentionDirectory.ts:12, 62, src/bundled/mentions/use-mention-choices.ts:29-215, src/bundled/mentions/mention-candidates.ts:35 | PARTIAL (crates/app/src/ui/composer.rs:750-785) | Uses members plus every cached named profile; there is no directory search, agent choices or archive exclusion. | M |
| Space accepts only an exact, unique name with no continuing name and a complete choice set | src/bundled/mentions/README.md:176-194 | PARTIAL (crates/app/src/ui/composer.rs:868-908) | Exactness is checked only among the shown rows; continuations (`Jose Luis`) and pending searches are ignored. | S |
| At most 50 rows with the status "Narrow your search to see more members." | src/bundled/mentions/MentionCompletion.tsx:171-178, 211-213 | PARTIAL (crates/app/src/ui/composer.rs:44, 801) | Truncates silently. Add the status line. | S |
| Completion status lines (Loading recipients…, Searching community…, membership unavailable, names unavailable) and Retry | src/bundled/mentions/MentionCompletion.tsx:189-235 | MISSING | Add a status/retry footer to the completion popup. | S |
| Disabled rows with a reason (unavailable recipient); Enter/Tab on them do not fall through to Send | src/features/conversation/ComposerCompletions.tsx:291-301, 377 | MISSING | Add a disabled state per suggestion. | S |
| Mention picker popover from the @ button: search field ("Search community people and agents"), list, keyboard highlight, "No matching channel members." | src/bundled/mentions/MentionPicker.tsx:150-399 | DIFFERENT (crates/app/src/ui/composer.rs:2063-2078) | The port's @ button only types "@" to open inline completion. Build the popover. | M |
| Saved teams: mention a whole team (avatars, "Saved team · N agents", outsider consequence), with team names blocking Space selection | src/bundled/mentions/use-team-mentions.ts:13-207, src/bundled/mentions/TeamMentionAvatars.tsx:6 | MISSING | Needs the channel-kit team catalog and batch insertion. | L |
| Batch insertion is atomic: "Choose at most 32 recipients" and the length limit ("The team would exceed…") | src/features/messages/MessageComposer.tsx:801-804, 825-885 | PARTIAL (crates/relay/src/messages.rs:449-456) | Only the writer rejects more than 32 at send. Check at insertion. | S |
| Recipient avatar spring scale-in animation | src/features/messages/MessageComposer.tsx:1756-1772 | MISSING | Add the 0.9 → 1 scale on appearance (skipped for keyboard navigation and reduced motion). | S |
| Namesake qualifiers on chips (`Honey · 4prr`), revealed when a second namesake joins | src/features/messages/RichComposerInput.tsx:73-107, 237-245 | MISSING | Use the contextual identity-name resolver for chip labels. | M |
| Recipient no longer admissible at insertion: "This recipient is no longer available. Remove it or refresh choices." | src/features/messages/MessageComposer.tsx:787-800, src/features/messages/mention-admission.ts:29-50 | MISSING | Admission check when a chip is inserted. | S |

### Channel and emoji completion, emoji picker

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Open (unjoined, public) channels from a search, after joined ones with 5 reserved slots, marked "Not joined" | src/bundled/channels/ChannelCompletion.tsx:96-117, 130-133 | MISSING | Search public channels and append them. | M |
| Duplicate channel names show the channel id in the detail; "Private channel" detail; "Enter" hint on the selected row | src/bundled/channels/ChannelCompletion.tsx:130-136, src/features/conversation/ComposerCompletions.tsx:422-426 | PARTIAL (crates/app/src/ui/composer.rs:2442-2462) | "Private channel" and the Enter hint are present; the duplicate-name id is missing. | S |
| Channel completion statuses (Loading channels…, Searching open channels…, Keep typing to narrow the list.) | src/bundled/channels/ChannelCompletion.tsx:156-175 | MISSING | Status footer (shared with the mention row). | S |
| Typing the closing colon accepts an exact shortcode; Space never accepts an emoji; a shortcode inside inline code stays literal | src/features/messages/MessageComposer.tsx:908-920, src/features/messages/MessageComposer.test.tsx:2861-2885 | PARTIAL (crates/app/src/ui/composer.rs:868-908) | The closing colon works. Add protection for code literals (including a Space-accepted mention inside code). | S |
| Shared shortcodes are labelled "Community emoji" / "Unicode emoji"; catalog error statuses with retry | src/bundled/emoji/EmojiCompletion.tsx:133, 149-177 | MISSING | Detail labels and status lines. | S |
| Custom emoji shortcodes, links and profile links render inline in the draft (decorations), with Shift+Arrow treating a rendered shortcode as one unit | src/features/messages/RichComposerInput.tsx:114-276, src/features/messages/emoji-selection.ts:2-33 | MISSING | Render the custom-emoji image, link chip and identity chip decorations in the textarea. Related to the live-styling work but separate from it. | M |
| Project resource picker inserts resource atoms ("Resource: …") at the caret | src/features/messages/MessageComposer.tsx:886-896, src/features/messages/EditableInput.tsx:1764-1772 | MISSING | Projects plugin tool plus atom tokens. May belong to the Projects audit. | M |

### Voice notes

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Errors: microphone permission ("Allow Buzz to access your microphone to record a voice note."), unavailable, interrupted, prepare failed; Close button | src/bundled/voice-notes/useVoiceNoteRecorder.ts:73-74, 131-142, 185, 240-250, src/bundled/voice-notes/VoiceCapture.tsx:45-56 | PARTIAL (crates/app/src/ui/media/recorder.rs:32-34, crates/app/src/ui/composer.rs:2210-2226) | The permission-denied copy is missing; every start failure says "Buzz could not start the voice recorder." | S |
| Waveform strip slides 5 px per new sample (unless reduced motion) | src/bundled/voice-notes/VoiceNoteRecorder.tsx:47-61 | MISSING | Animate the bar strip offset between level ticks. | S |
| Discard button takes focus on start; Close focuses on error; focus returns to the composer after accept/cancel | src/bundled/voice-notes/VoiceNoteRecorder.tsx:27-29, src/bundled/voice-notes/VoiceCapture.tsx:17-19, src/bundled/voice-notes/index.tsx:25-33 | PARTIAL (crates/app/src/ui/composer.rs:1191-1199) | Only discard refocuses the composer. | S |
| Voice `waveform` written into the imeta tag | src/features/relay/attachments.ts:323-335 | DIFFERENT (crates/relay/src/messages.rs:271-273) | Intentional in the port: the relay refuses the `waveform` key, so only `duration` is written. | S |

## 5. Rendered attachments, media viewer, audio/video and voice-note playback

Note: Main gaps: no inline video playback (play opens the viewer), no timecoded-reply parsing (no time chips, timeline markers or quick reactions at the current frame), no shared-element open/close or gallery slide animation, no "Copy image", no download error notices, no snapshot attachments, and media keeps playing when the conversation is hidden.

Counts: MISSING 33, PARTIAL 14, DIFFERENT 17, IN PROGRESS 0, DONE 68.

### Attachment layout, thumbnails and placeholders

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| The image strip is one horizontal row that scrolls (`overflow-x:auto`) and never wraps. | src/features/messages/Messages.module.css:434 | DIFFERENT (crates/app/src/ui/message.rs:734) | The port uses `flex_wrap`, so large groups wrap onto more rows. Make it one non-wrapping row that scrolls horizontally (padding 4px, gap 8px). | S |
| Smooth "squircle" media corners (60% smoothing path clip and SVG outline) on tiles, video previews, inline controls and the voice card | src/features/messages/media-corners.ts:1; use-media-corners.ts:4 | DIFFERENT (crates/app/src/ui/message.rs:782; media/video.rs:127; media/audio.rs:509) | The port uses plain circular-arc `rounded()`. Add a smoothed-corner path paint/clip, or accept the difference. | M |
| The original replaces the blurhash immediately once it is decoded. | src/features/messages/AttachmentImage.tsx:193-207 | DIFFERENT (crates/app/src/ui/media.rs:184-203) | The port cross-fades over 180ms. This is a cosmetic difference. Either drop the fade or keep it on purpose. | S |
| If the original fails to load, the tile keeps the blur/placeholder. "Image unavailable" appears only when the URL cannot be resolved. | src/features/messages/AttachmentImage.tsx:61-69, 158 | DIFFERENT (crates/app/src/ui/media.rs:180) | The port shows the "Image unavailable" tile on any fetch or decode error. Match the reference: keep the blurhash or placeholder when loading fails. | S |
| Thumbnail labels ("Open image N of M"), keyboard focus ring, and the strip scrolls a keyboard-focused tile into view | src/features/messages/MessageRow.tsx:120-140, 719-722; Messages.module.css:449 | MISSING | Make tiles focusable with a focus ring. Scroll the strip so the focused tile is visible. Add the accessible label. | S |
| Any format the browser decodes renders (for example AVIF and SVG). | src/features/messages/AttachmentImage.tsx:204 | PARTIAL (crates/app/src/ui/media.rs:351-370) | The port sniffs only PNG, JPEG, GIF, WebP, BMP, TIFF and ICO. AVIF and SVG show as unavailable. Add AVIF, and add SVG if wanted. | S |
| Non-thumbnail image preview (`MediaAttachment` image mode, min(360, w, 320*w/h)) | src/features/messages/MediaAttachment.tsx:134-189; AttachmentImage.tsx:42-51 | MISSING | This is only used by surfaces outside the timeline (GitHub plugin). It is needed only if those surfaces are ported. | S |
| Attachment renderer registry: plugins can claim attachments, with a fallback renderer | src/features/conversation/AttachmentView.tsx:8-36; MessageRow.tsx:684 | DIFFERENT (crates/app/src/ui/message.rs:719-723) | The port hard-codes voice-note vs plain audio. Add a renderer registry only if plugin attachment renderers are in scope. | M |

### File attachment card

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Icon is always `file-text` (24px). | src/features/messages/FileAttachment.tsx:51 | DIFFERENT (crates/app/src/ui/message.rs:827-831) | The port uses VideoCamera or SpeakerHigh for audio/video shown as file cards. Use FileText to match. | S |
| A relay file click saves to Downloads without overwriting (host `media_download`) and does not open the file. | src/features/messages/FileAttachment.tsx:93-98; src-tauri/src/relay.rs:1579-1600 | DIFFERENT (crates/app/src/ui/media.rs:375-401) | The port also opens the saved file with the system handler. Drop `open_with_system`, or show a "Saved to Downloads" notice instead. | S |
| "Download failed" alert when a download fails | src/features/messages/FileAttachment.tsx:102 | MISSING (crates/app/src/ui/media.rs:381, 397 only log) | Surface the failure inline next to the card. | S |

### Snapshot attachments

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Agent/team snapshot attachment: a file card plus an "Add agent" or "Add team" button that opens the snapshot import preview. The button is disabled when the message is cached or has no source. | src/features/messages/SnapshotAttachment.tsx:12-48; MessageRow.tsx:660 | MISSING | Detect snapshot attachments (`snapshotAttachmentKind`) in rows. Render the button and route it into the agents import flow (crates/app/src/ui/agents/import.rs). | M |
| Snapshot preview error alert (message or "Snapshot preview is unavailable.") | src/features/messages/SnapshotAttachment.tsx:30-38, 47 | MISSING | Show the error inline under the card. | S |

### Inline video preview (timeline)

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Plays inline in the timeline. A centered play button plays in place, with an inline media-glass control bar (volume, time, timeline, duration, speed). The bar fades while playing unless hovered or focused. | src/features/messages/MediaAttachment.tsx:198-280; VideoPlayer.tsx:63-418; VideoPlayer.module.css:262-330 | DIFFERENT (crates/app/src/ui/media/video.rs:77-195) | In the port, play opens the viewer and the preview shows a duration chip instead of controls. Render engine frames inline and reuse the player controls with the `inline` styling. | M |
| An expand button (top right) is revealed on hover or focus. It pauses the video and opens the review at the current time. | src/features/messages/MediaAttachment.tsx:281-295; Messages.module.css:1976 | PARTIAL (crates/app/src/ui/media/video.rs:155-168) | Only hover reveals it, and the review always opens at 0. Add focus reveal and the current-time handoff once inline playback exists. | S |
| "Video unavailable" status when the source is missing or fails to decode | src/features/messages/MediaAttachment.tsx:116-132 | MISSING (crates/app/src/ui/media/video.rs:101) | Probe errors are dropped and the preview stays a blank dark box. Show the unavailable status. | S |
| Inline video seeks to a timecode when a timecode chip in the thread panel is clicked (`seekTo` / `seekRequest`). | src/features/messages/MediaAttachment.tsx:42-70; ThreadPanel.tsx:1024-1027 | MISSING | Needs inline playback and timecode chips (see below). | M |
| Inline playback position is reported to the host (`onPlayback`) and synced back from the viewer to the inline element. | src/features/messages/MediaAttachment.tsx:248-252, 318-322 | MISSING | Needs inline playback. | M |
| Inline video, audio and voice notes pause, and the viewer closes, when the conversation is no longer presented (tab or channel switch). | src/features/messages/MediaAttachment.tsx:56-58, 92; AudioAttachment.tsx:47-49; VoiceNoteCard.tsx:59-65 | MISSING (crates/app/src/ui/media/audio.rs:33-50) | The global audio track keeps playing after you navigate away. Stop or pause the track when the channel/thread view changes. | S |

### Video player in the viewer

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Autoplays on open, starting at the time handed off from the inline preview | src/features/messages/VideoPlayer.tsx:461, 468-480 | PARTIAL (crates/app/src/ui/media/video.rs:286-287) | Autoplays, but always from 0. Accept an initial time. | S |
| Notice "Playback paused. Press play to try again." when play() is rejected | src/features/messages/VideoPlayer.tsx:153-161, 184-188 | MISSING | Show the notice when the engine refuses to play. | S |
| Error state reads "Media unavailable". | src/features/messages/MediaReviewViewer.tsx:419-430 | DIFFERENT (crates/app/src/ui/media/video.rs:703-722) | The port shows "Can't play this video here" and an "Open externally" button, which the port added for missing codecs. Align the copy or keep it on purpose. | S |
| Volume: speaker button that opens a hover popover with a vertical slider and a Mute/Unmute button. The icon reflects the muted state. | src/features/messages/VideoPlayer.tsx:197-281; VideoPlayer.module.css:180-260 | DIFFERENT (crates/app/src/ui/media/video.rs:523-548) | The port shows a separate mute button and an always-visible 64px horizontal slider. Move to a hover popover with a vertical slider. | S |
| Hover time preview bubble above the timeline (mouse only) | src/features/messages/VideoPlayer.tsx:283-329 | MISSING (crates/app/src/ui/media/seek.rs:18-63) | Track the hover position in `seek_area` and draw the time label. | S |
| Keyboard-operable timeline and volume sliders (focusable, arrow keys) | src/features/messages/VideoPlayer.tsx:242-260, 301-317 | MISSING (crates/app/src/ui/media/seek.rs:18) | Make the seek areas focusable and step them with the arrow keys. | S |
| Timecoded-comment markers on the timeline (author avatars). Clicking one seeks, opens the comments and selects that comment. | src/features/messages/VideoPlayer.tsx:330-358; MediaReviewViewer.tsx:243-264, 444-447 | MISSING | Parse timecoded replies and overlay avatar buttons on the timeline. | M |
| Chosen speed is persisted (`buzz.video.playback-speed`) and broadcast to every open player | src/features/messages/VideoPlayer.tsx:47-57, 113-126, 392-404 | MISSING (crates/app/src/ui/media/video.rs:268) | Every player starts at 1x. Persist the speed in settings and share it as a global. | S |
| Controls hide while playing when the pointer leaves or is idle for 2.2s. Focus or open menus keep them visible. | src/features/messages/use-media-controls.ts:4-35; VideoPlayer.module.css:307-315 | PARTIAL (crates/app/src/ui/media/video.rs:501-504) | The port only hides them when not hovered. Add the idle timer and keep them visible while focused or while a menu is open. | S |
| On narrow players (<25rem) the timeline wraps to its own row. | src/features/messages/VideoPlayer.module.css:332-340 | MISSING | Add the width-based layout. | S |
| Press and hold on the right half plays at 2x until release | src/features/messages/use-video-gestures.ts:123-147 | MISSING | Add a hold timer (350ms) on mouse down, set rate 2, and restore on release. | S |
| Gesture feedback overlay (play/pause icon, rotate-arrow with "10", "2x") fading after 650ms | src/features/messages/VideoPlayer.tsx:484-509; use-video-gestures.ts:29-34 | MISSING | Draw a centered transient badge. | S |
| Progressive streaming: relay media streams with range reads through a local listener, so playback starts before the download finishes. | src/features/messages/use-media-element-source.ts:1-51 | DIFFERENT (crates/app/src/ui/media/engine.rs:4-7; crates/relay/src/media.rs:22) | The port downloads the whole file into memory (100 MB cap) before decoding, and the inline preview probe also fetches the whole video. Add a range/streaming appsrc. | M |
| "Fill the window" expand toggle (Escape exits first) | n/a (port only) | DIFFERENT (crates/app/src/ui/media/video.rs:615-631; review.rs:237) | The reference has no such control. Keep it as a deliberate extra or remove it. | S |

### Video review reactions and timecoded comments

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Quick reaction bar under the player (😂 😍 😮 🙌 👍 👎). It pauses the video and posts the reply `⏱ m:ss — <emoji>`. | src/features/messages/VideoReviewReactions.tsx:46-77 | MISSING | Add the bar inside the player chrome and send through the thread reply path. | S |
| Custom emoji picker (ReactionTool) in the same bar | src/features/messages/VideoReviewReactions.tsx:72-82 | MISSING | Reuse the emoji picker. | S |
| Bar disabled in read-only channels. Error alert "Could not post reaction." | src/features/messages/VideoReviewReactions.tsx:30-33, 44-50, 84 | MISSING | - | S |
| Timecode stamp format: `formatMediaTime` (m:ss, h:mm:ss from one hour). The time readout next to the checkbox is padded 00:00 with tabular figures. | src/features/messages/media-timecode.ts:9-21; MediaReviewViewer.tsx:517 | DIFFERENT (crates/app/src/ui/media/comments.rs:21-33, 285-289) | The port always writes m:ss (3725s becomes "62:05") and shows the readout as unpadded m:ss in a mono font. Reuse `media_time` and `video_time`. | S |
| Focusing the comment composer pauses the video | src/features/messages/MediaReviewViewer.tsx:525-530 | MISSING | Pause the player when the composer gains focus. | S |
| Timecoded replies (`⏱ m:ss — text` and legacy `[m:ss]`) render with the prefix stripped and a mono time chip before the body. | src/features/messages/MessageRow.tsx:257-258, 630-643; media-timecode.ts:23-67; Messages.module.css:1962-1970 | MISSING | The port has no parser, so rows show the raw "⏱ 0:05 — …" text. Add `parse_media_time_reply` and the chip. | S |
| Clicking a time chip in the viewer comments seeks the video and plays | src/features/messages/MediaReviewViewer.tsx:299-309, 627 | MISSING | Wire the chip click to `VideoPlayer::seek_to` and play. | S |

### Media review viewer: shell, motion, states

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Narrow windows: backdrop padding 12px (<50rem) and comments shown as a bottom sheet (<51.25rem) | src/features/messages/Messages.module.css:2027-2031, 2143-2157 | MISSING | Add width breakpoints to the review layout. | S |
| Shared-element open animation: the picture flies from the thumbnail with a smooth-corner clip (240ms). Chrome and scrim fade in. Respects reduced motion. | src/features/messages/use-review-entrance.ts:20-340; MediaAttachment.tsx:146 | MISSING | Capture the thumbnail bounds on open and animate the picture's bounds and corner radius. | M |
| Close animation: the picture flies back to its thumbnail, or fades if the thumbnail is off-screen, zoomed or a different gallery image. Chrome fades in 100ms and the scrim fades out. | src/features/messages/use-review-entrance.ts:341-499 | MISSING | Close is instant now. Add the exit animation. | M |
| The comments sidebar slides in and out (200ms) while the picture moves smoothly to its new bounds. | src/features/messages/use-review-sidebar-motion.ts:28-103; Messages.module.css:2126-2139 | MISSING | Animate the sidebar width and offset. | S |
| States: thread loading, thread error with Retry, "Original message unavailable." with Retry, "Attachment unavailable.", stage "Media unavailable" with Retry | src/features/messages/MediaReviewViewer.tsx:133-167, 419-430, 547-549 | PARTIAL (crates/app/src/ui/media/comments.rs:195-206) | Status text appears only in the sidebar. There is no Retry button, no check that the attachment is still in the thread, and the stage never shows thread errors. | S |

### Image gallery stage (viewer)

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Gallery holds every image across the thread (root and replies), de-duplicated | src/features/messages/MediaReviewViewer.tsx:267-276 | PARTIAL (crates/app/src/ui/media/review.rs:72-80) | The port uses only the opened message's images. Build the gallery from the thread snapshot. | S |
| Slide and fade between images (200ms, ±40px). The last decoded image stays visible while the next loads. Respects reduced motion. | src/features/messages/use-image-gallery-motion.ts:15-122 | MISSING | Keep the departing image and animate both images. | S |
| Only a click on the picture itself zooms (not the letterbox or a second touch) | src/features/messages/ImageReviewStage.tsx:247-253 | PARTIAL (crates/app/src/ui/media/review.rs:428-437) | Any press on the stage counts as a click. Hit-test against the image rect. | S |
| Trackpad pinch-to-zoom (gesture events) | src/features/messages/use-image-viewport.ts:165-186 | MISSING | Handle pinch/magnify events if GPUI exposes them. | S |
| Pan is re-clamped on window resize or sidebar toggle | src/features/messages/use-image-viewport.ts:187-193 | PARTIAL (crates/app/src/ui/media/review.rs:277-293) | The offset is clamped only on the next gesture, so it can go stale after a resize. Re-clamp when the stage bounds change. | S |
| Cursors: zoom-in, grab, grabbing | src/features/messages/Messages.module.css:1192-1200 | DIFFERENT (crates/app/src/ui/media/review.rs:407-413) | The port uses PointingHand, OpenHand and ClosedHand. Use a zoom-in cursor if the platform has one. | S |
| Toolbar auto-hides after 2.2s of idle pointer and stays visible during copy feedback | src/features/messages/ImageReviewStage.tsx:106-107; use-media-controls.ts:4 | PARTIAL (crates/app/src/ui/media/review.rs:504-505) | The port shows the toolbar only while the stage is hovered. Add the idle timer and keyboard visibility. | S |
| "Copy image" button copies authenticated PNG bytes to the clipboard, with a spinner and "Image copied" / "Couldn't copy image" notices (4s/6s) | src/features/messages/ImageReviewStage.tsx:117-158, 384-400; image-copy.ts:1-37; native-image-copy.ts:1-6 | MISSING | Fetch through `Media` and write the image to the clipboard. Add the notice. | S |
| "Download image" for relay media saves to Downloads | src/features/messages/ImageReviewStage.tsx:401-429 | DIFFERENT (crates/app/src/ui/media/review.rs:608-615; media.rs:375-401) | The port also opens the saved file afterwards. Match the reference's silent save, or add a confirmation notice. | S |
| "Download failed. Try again." alert | src/features/messages/ImageReviewStage.tsx:416-418 | MISSING | Report errors from `media::download`. | S |

### Comments sidebar

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Comments are full message rows (reactions, hover action bar, edit via MessageEditScope, retry, day dividers) | src/features/messages/MediaReviewViewer.tsx:502-514, 601-628 | PARTIAL (crates/app/src/ui/media/comments.rs:175-186) | Rows are built with empty `Actions`, no picker and no quick reactions, so comments cannot be reacted to, edited or retried. | M |
| Clicking an attachment inside a comment switches the current viewer to that media (same review, seek re-applied) | src/features/messages/MediaReviewViewer.tsx:95-100, 624-626 | DIFFERENT (crates/app/src/ui/message.rs:799; media/review.rs:61-104) | The port opens a new review for the reply message, which replaces the current one. Switch the media inside the same review. | S |
| The comment selected from a marker is outlined and scrolled into view | src/features/messages/MediaReviewViewer.tsx:581-586, 605; Messages.module.css:2002-2005 | MISSING | Needs the markers. Add selected-comment state, scroll and outline. | S |

### Audio attachment player

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Duration is known before play (preload metadata) | src/features/messages/AudioAttachment.tsx:250-253 | PARTIAL (crates/app/src/ui/media/audio.rs:68-86) | Without an imeta duration the port shows "—:—" and disables seeking until playback starts. Probe the metadata lazily. | S |
| Hover time preview on the seek bar | src/features/messages/AudioAttachment.tsx:310-361 | MISSING (crates/app/src/ui/media/seek.rs:18) | Same work as the video hover preview. | S |
| Duration is checked (finite, at most the maximum attachment duration). Old voice notes with an overshooting container duration are corrected when they end. | src/features/messages/AudioAttachment.tsx:152-164, 228-240, 266-288 | PARTIAL (crates/app/src/ui/media/audio.rs:310-312) | The port trusts the engine duration. Add the cap and the end-of-clip correction. | S |

### Voice note card (playback)

| Feature | buzz-app reference | Port status | What's needed | Size (S/M/L) |
|---|---|---|---|---|
| Loading spinner rotates | src/bundled/voice-notes/VoiceNoteCard.tsx:190-194; VoiceNotes.module.css:254 | PARTIAL (crates/app/src/ui/media/audio.rs:481) | The CircleNotch icon is static. Add a rotation animation. | S |
| Bar count follows the card width (floor((w+2)/5), at most 64) | src/bundled/voice-notes/VoiceNoteCard.tsx:80-91 | PARTIAL (crates/app/src/ui/media/audio.rs:30) | The port always draws 42 bars. Measure the track width. | S |
| Seeking on the waveform is enabled only after metadata loads (that is, after Play) | src/bundled/voice-notes/VoiceNoteCard.tsx:217-234 | DIFFERENT (crates/app/src/ui/media/audio.rs:166-185, 457-462) | The port lets you seek before playing and loads the clip paused at that time. This is harmless and could stay. | S |

## Not counted: absent from buzz-app, or not applicable to a native app

These were checked so that nobody ports them by mistake. In each case the port already matches.

- Code blocks: no syntax highlighting, no copy button (reference has neither): `src/features/messages/MessageMarkdown.tsx:618-630`
- No OpenGraph cards, no YouTube/media embeds for links (reference has none): `src/bundled/link-lab/README.md:9-13`
- Mac WebKit workarounds (scroll-momentum fix, ghost caret): `src/features/messages/mac-webkit.ts:1-6`
- Right-click context menu on messages (none in buzz-app; no onContextMenu in src/features/messages)
- Pin message (not in buzz-app)
- Fallback reaction text row when the conversation has no session/extensions: `src/features/messages/MessageRow.tsx:810-840`
- Picker load error "Could not load emoji picker: …" + "Retry picker": `src/bundled/emoji/EmojiPicker.tsx:263-270`
- "Reactions unavailable" status when the picker contribution crashes: `src/features/conversation/ReactionTool.tsx:44-47`
- Rename / delete custom emoji (not in buzz-app)
- Unread "new messages" divider line in the timeline (docs/port/ui.md (4.3) / src/features/messages/ChannelTimeline.tsx:763)
- Last reply time in the summary: `src/features/messages/ReplySummary.tsx:61`

## DONE count

DONE: **265** features already at parity (section 1: 43, section 2: 51, section 3: 61, section 4: 42, section 5: 68).

Status counts: MISSING 199, PARTIAL 88, DIFFERENT 66, IN PROGRESS 4, DONE 265.

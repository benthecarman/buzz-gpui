# Parity gap audit: channels, communities, DMs and the relay data layer

- Reference: buzz-app at `/home/ben/projects/buzz-app` (React/Tauri).
- Port: buzz-gpui at `/home/ben/projects/buzz-gpui`, main checkout only. `.claude/worktrees/` was not audited.
- Slice: `src/features/{channels,channel-members,channel-navigation,channel-templates,communities,direct-messages,relay}`; `src/bundled/{channels,channel-templates,channel-usage,hosted-communities,moderation,inbox,profiles}`; `src-tauri/src/relay*` and `src-tauri/src/enterprise_*`.
- Citations: buzz-app paths are relative to the buzz-app root. Port paths are relative to the buzz-gpui root (`crates/...`), unless a section says otherwise. Line numbers were read on 2026-10-09.
- Status: MISSING, PARTIAL (the gap is described), DIFFERENT (the difference is described), DONE. DONE rows have "-" in "What's needed".
- Duplicates: when a behaviour belongs to more than one area, it is listed once and the other areas point to it.

## Summary

| Area | DONE | PARTIAL | DIFFERENT | MISSING | Rows |
|---|---|---|---|---|---|
| Connection, transport, native host, enterprise | 24 | 14 | 16 | 12 | 66 |
| Session, caching, windows, folding | 39 | 14 | 3 | 14 | 70 |
| Outbox, publishing, attachments, media | 27 | 10 | 11 | 16 | 64 |
| Read state and unread | 38 | 11 | 6 | 8 | 63 |
| Sidebar, discovery, navigation, DMs; members, moderation, profiles; communities, hosted, inbox | 46 | 16 | 6 | 67 | 135 |
| Other relay-layer features | 36 | 12 | 8 | 35 | 91 |
| Channel page, settings, lifecycle, create, canvas, templates, usage | 13 | 16 | 7 | 37 | 73 |
| **Total** | **223** | **93** | **57** | **189** | **562** |

### Highest-impact findings

1. **The sidebar preferences layer is not wired up.** `crates/relay/src/sidebar.rs` (3.7k lines) ports registers, edits, decoding, sections, local hidden DMs, activity and personal groups. Only its own tests call it. The UI sidebar is a fixed Channels / Forums / Direct messages list: no Starred, groups, sort, mute, collapse, drag, row context menu or resize.
2. **Nothing is persisted to disk.** No disk heads, startup roster cache or offline resume. `Store::save_head`/`load_head` and the `startup_cache` table have no callers. A community shows Connecting with an empty sidebar until NIP-11 succeeds.
3. **AUTH and reconnect behaviour differs.** After 1.5 s with no AUTH challenge, the port treats the relay as open and sends REQs (`crates/relay/src/socket.rs:348-351`). AUTH refusals and signing failures are not terminal. Reconnect retries forever with jitter instead of 5 attempts and then a manual retry. `retry()` clears the relay's quota cooldown.
4. **Connection and live status are invisible.** The snapshot exposes only `live: bool`. `Session::retry` has no caller in `crates/app`. There is no "Live updates need attention" toast, no failed-connection banner, and no sidebar connecting or error states.
5. **Some sends can be invisible.** Only signed outbox ops reach timelines (`crates/relay/src/session/mod.rs:1462-1468`). The outbox exists only after connect. There is no outbox panel, and failed edits, deletes and reactions disappear from the timeline.
6. **DMs are counted as mentions.** DM entries count as mentions (`crates/relay/src/unread.rs:1536-1538`). DM rows get a numeric pill, and the tray counts DMs twice. The sidebar also shows a numeric pill where buzz-app shows a dot.
7. **Whole features are absent:** community moderation (Membership card, invites), hosted communities, the inbox page, the profile panel, Canvas, channel templates, channel usage, reminders, identity archives, the tab pane and panels, and HEIC/video preparation and media streaming.
8. **Some writes can be resent.** An unconfirmed lifecycle action can be sent again, and an uncertain channel-details save has no retained state. Renaming a session channel fails because the port always sends `about`. There is no temporary-channel TTL.

## Connection, transport and native relay host

Biggest gaps: no connection or live-status UI (session exposes only `live: bool`; nothing in `crates/app` calls `Session::retry`); NIP-42 AUTH failures are not terminal (a refused or failed AUTH loops forever, or after 1.5 s REQs go out unauthenticated); reconnect retries forever with jitter instead of 5 attempts then manual retry; several quota branches differ; offline cached startup is missing; several host features (observer route, workflow runs, git auth, channel kit, catalog guard, enterprise login gate) are absent. Paths: buzz-app `src/features/relay/` or `src-tauri/src/`; port `crates/relay/src/`.

### Session connection lifecycle

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Connect reads NIP-11: `self` must be 64-hex ("Relay did not advertise its identity"); NIP-29 enables channel creation; read-state snapshot descriptor | native.ts:315-342; src-tauri/src/relay.rs:610-638 | DONE (session/mod.rs:831-844, 1580-1605) | - | - |
| 8 s connect deadline "Relay connection timed out. Try again." | service.ts:187-197 | DONE (session/mod.rs:836-840) | - | - |
| A failed connect waits for user retry (or online/visible while cached) | service.ts:165-302 | DIFFERENT | Port auto-retries after 2/4/8/16/32/32 s forever (session/mod.rs:846-861); drive retries from UI + online trigger | S |
| OS `online` retries a cached session; becoming visible runs `refreshList()` | service.ts:341-363 | MISSING | Network reachability and window activation hooks | S |
| Native origin must be HTTPS without credentials/path/query/fragment | src-tauri/src/relay.rs:58-72 | DIFFERENT | Port also accepts `ws://`/`http://` (origin.rs:270-291); decide for non-dev builds | S |

### Socket open and NIP-42 AUTH

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| One socket per (origin, viewer); URL `wss://host` | transport.ts:1172-1205; live.ts:204-214 | DONE (live.rs:157-174; origin.rs:293) | - | - |
| AUTH kind 22242 with exactly `relay` + `challenge`, challenge 1–4096 bytes | live.ts:720-747; src-tauri/src/relay.rs:260-285 | DONE (crates/identity/src/lib.rs:223-234; socket.rs:578-592) | - | - |
| Every REQ held until AUTH OK; never treated as open relay | live.ts:523, 748-760 | DIFFERENT | No challenge within 1.5 s marks relay open and sends REQs (socket.rs:90, 348-351, 383-394); remove or dev-only | S |
| 10 s auth deadline from socket creation ("Live authentication timed out") | live.ts:698-701 | PARTIAL | Deadline only after AUTH is sent (socket.rs:352-353, 418-420) | S |
| `OK false` for AUTH is terminal ("Relay rejected live authentication") | live.ts:748-752, 679-686 | DIFFERENT | Logged (socket.rs:558-566), times out, reconnects forever; add terminal error state | S |
| Signing failure / signer ≠ viewer is terminal | live.ts:736-745 | DIFFERENT | Logged (socket.rs:587-590); 1.5 s fallback then sends unauthenticated REQs | S |

### Reconnect, liveness, recovery

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Backoff `500*2^n` ms, no jitter | live.ts:677 | DIFFERENT | 500 ms·2^(n-1) capped 8 s with 50–100 % jitter (socket.rs:93-94, 654-666) | S |
| ≤5 auto-reconnects then "Live reconnect attempts exhausted; retry available"; counter resets only in `retry()` | live.ts:671-677, 1055-1058 | DIFFERENT | Unbounded; counter resets on every Ready (socket.rs:274-292, 372-375) | S |
| No client heartbeat | live.ts (absent); docs/port/relay.md:700-701 | DIFFERENT | Port pings every 25 s, reconnects after 70 s silence (socket.rs:91-92, 412-417); keep and document | S |
| On reconnect rebuild routes with `since=now-300`, `limit 500` | live.ts:653-660, 430 | DONE (live.rs:295-317, 408-421) | - | - |
| Live `retry()` keeps the server cooldown | live.ts:1055-1069, 24-59 | DIFFERENT | `retry()` sets `cooldown = None` (live.rs:247-260), skipping the quota pause | S |
| 60 s tick renews multi-channel batches with `limit:0` | live.ts:345-368 | DONE (live.rs:743-762) | - | - |
| Tick re-pends retained renewals and calls `recover()` (roster refresh + `unread.reconnect()`) | live.ts:353-367 | MISSING | `recover()` emits no LiveUpdate; no periodic roster/unread repair | M |

### Subscription management

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Routes (profiles, membership, presence, singletons, batches of 10); capacity 1022; `limited` over capacity | live.ts:17-22, 369-454 | DONE (live.rs:40-54, 321-406) | - | - |
| REQ filters per route (channel kinds `#h`; profiles; membership incl. read-state and 30300; presence `limit 0`) | live.ts:200-203, 572-635 | DONE (live.rs:462-511) | - | - |
| Observer/telemetry route (24200/44200 `#p viewer`, archive capture, settings retry) | live.ts:302-320, 567-588, 852-866; native.ts:539-659 | MISSING | With agent activity feature | M |
| `prioritize` ≤64 ids; singletons; globals first; `SETUP_CONCURRENCY=4` | live.ts:22, 521-553, 1031-1035 | DONE (live.rs:214-226, 515-565) | - | - |
| Setup deadline 10 s ("Live subscription setup timed out; retry available"); keep previous wire and re-pend | live.ts:455-473, 558-565 | PARTIAL | With a previous wire the route is marked `Live` without error and never renewed again (live.rs:714-738) | S |
| Replacement keeps previous wire until EOSE; previous-wire events filtered to narrowed scope | live.ts:323-344, 832-842 | PARTIAL | Previous-wire events use the old wider scope (live.rs:578, 643-659) | S |
| Interest validation (≤1024 ids, id regex, presence authors 64-hex ≤256, throw not truncate) | live.ts:148-181 | PARTIAL | No validation; presence authors truncated (live.rs:214-236) | S |
| Dropping a route fences its wire then sends CLOSE | live.ts:288-297 | DONE (socket.rs:167-171, 453-460; live.rs:436-441) | - | - |

### Frame handling

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Inbound frames over 1 MiB or non-string dropped | live.ts:707-710 | PARTIAL | Binary ignored (socket.rs:364-367); no size cap; set `max_message_size` 1 MiB | S |
| Invalid live EVENT fails the route ("Relay supplied invalid live traffic") | live.ts:805-811 | DIFFERENT | Dropped with warning (socket.rs:509-516) | S |
| Channel attribution incl. multi-`h` events delivered with `sourceChannels` | live.ts:812-879 | PARTIAL | Multi-`h` events dropped (event.rs:173-185; live.rs:647-652) | S |
| Replay vs live phase tagging | live.ts:870-878 | DONE (live.rs:579-583) | - | - |
| EOSE: route live, previous wire closed, `established`; `replay:"limited"` when ≥500 | live.ts:880-892 | PARTIAL | `replayed` count (live.rs:124, 584-588) never exposed | S |
| CLOSED: reason ≤512 chars; "restricted: not a channel member" = denied | live.ts:504-518, 893-903 | DONE (live.rs:664-697) | - | - |
| CLOSED on previous wire closes only that wire | live.ts:797-802 | DONE (live.rs:613-622) | - | - |
| NOTICE logged; OK matched to AUTH, presence, publications | live.ts:718, 748-790 | DONE (socket.rs:547-597) | - | - |

### Live quota and rate limits

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Shared cooldown `pause(N)` = now+(N+1) s, gates REQ dispatch, survives socket replacement; ≤3 quiet retries for hint ≤60 s | live.ts:23, 26-59, 475-492 | DONE (live.rs:516, 675-683, 700-712) | - | - |
| Missing hint treated as 5 s and retried | live.ts:479 | DIFFERENT | Route errors immediately "retries exhausted" (live.rs:684-689) | S |
| Hint >60 s: all unsent pending routes error "Unsupported live cooldown; automatic setup stopped"; pause min(N, 86400) | live.ts:480-489 | DIFFERENT | Only this route errors; pause capped 60 s (live.rs:684-689) | S |
| Retries exhausted stops every unsent pending route | live.ts:492-501 | PARTIAL | Only failing route stops | S |
| Transient CLOSED on renewal keeps wire and re-pends next tick | live.ts:455-472, 353-360, 898-902 | PARTIAL | `error:` routes stay in Error; never re-pended (live.rs:698-699, 743-762) | S |

### Socket publications and presence

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Socket publications (≤32 pending, ≤3 unacked, dup refused, 64 KiB frame, 10 s incl. pre-auth, receipt ≤16 KiB, `sent` kept) | socket-requests.ts:26-140 | PARTIAL | 15 s timeout and correlation only; pre-auth publish = NotConnected; all pending = Unknown on loss (socket.rs:89, 430-433, 461-472) | S |
| OK false proves non-delivery only for specific prefixes | socket-requests.ts:109-130 | DIFFERENT | Any OK false = Rejected; `duplicate:` = accepted (socket.rs:567-575) | S |
| Publication `rate-limited` pauses the shared live cooldown | live.ts:778-790 | MISSING | Presence refusals pause only presence (presence.rs:558-561) | S |
| Presence publish: 5 s gate, 10 s receipt timeout; no-hint quota pauses 86400 s | live.ts:922-985, 761-777 | PARTIAL | No-hint pause capped at 60 s (presence.rs:35) | S |
| Presence snapshot over `/query` (relay-signed, one `p`, 10 s, 1 MiB, absent = offline) | transport.ts:310-353, 1131-1171 | DONE (session/presence.rs:115-147; presence.rs:137-180) | - | - |

### HTTP transport, NIP-98 and admission

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| No redirects, 10 s connect, 30 s total; NIP-11 GET with `Accept: application/nostr+json` | src-tauri/src/relay.rs:640-653, 708-769; native.ts:99-124 | DONE (http.rs:65-70, 164-180) | - | - |
| NIP-98 kind 27235 (`u`, `method`, `payload`, `nonce`) per request | src-tauri/src/relay.rs:717-756 | DONE (crates/identity/src/lib.rs:238-257; http.rs:121-131) | - | - |
| Size caps: 16 MiB response, 1 MiB request | src-tauri/src/relay.rs:40-41, 662-666 | DIFFERENT | 8 MiB response (http.rs:11), no request cap | S |
| One invalid event rejects the whole read | transport.ts:221-243 | DONE (reader.rs:538-549; `RelayHttp::query` drops silently, http.rs:91-99) | - | - |
| API lane: 6 concurrent per (origin, viewer) | http-admission.ts:148; host-admission.ts:6-38 | DONE (http.rs:49-61, 111-120) | - | - |
| Lane queue: foreground first, ≤128 queued ("Relay API queue capacity reached") | http-admission.ts:139-154, 173-182, 204 | PARTIAL | FIFO semaphore, unbounded waiters (http.rs:113-118) | S |
| 429 quota pauses lane N+1 s (no hint 61 s, max 86400), fails queued work "Relay requests paused; retry in Ns" | http-admission.ts:34-51, 188-196, 242-264 | DONE (http.rs:136-161, 229-237) | - | - |
| Presence lossy lease outside the 6 slots | http-admission.ts:157-169 | PARTIAL | Presence reads take a normal slot (session/presence.rs:121) | S |
| Error text (4 KiB body, known messages else "Relay request failed (N)", error kinds) | http-admission.ts:31-113; errors.ts:1-36 | DONE (reader.rs:60-125) | - | - |

### Write receipts

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|

### Error surfaces

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Live updates need attention" toast with reason and "Retry live updates"; quiet during quota recovery; diagnostics and "Last rejection" | src/bundled/channels/LiveStatus.tsx:22-98 | MISSING | Snapshot only has `live: bool` (session/mod.rs:96-97); expose route status/errors, build toast | M |
| Relay connection error with Try again | service.ts:187-197, 290-295 | MISSING | `SessionStatus::Failed` never rendered; `Session::retry` (session/mod.rs:324-326) has no caller | S |

### Native relay host

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Webview hardening (path allowlist, sign-kind allowlist, `/events` kind gates, shape checks) | src-tauri/src/relay.rs:74-94, 225-323, 654-706 | DIFFERENT | No untrusted webview; signing scoped by `Purpose` (crates/identity/src/lib.rs:396) | - |
| Workflow runs read `GET /workflows/{id}/runs?limit=20` | src-tauri/src/relay.rs:96-162; native.ts:401-426 | MISSING | With workflows feature | M |
| Git authorization (27235 token) and project git | src-tauri/src/relay.rs:164-223; native.ts:427-455, 702-710 | MISSING | With projects feature | M |
| Catalog sign guard (30175/30178, `env_vars` refused) | src-tauri/src/relay/catalog.rs:1-87 | MISSING | With the agent catalog | M |

### Enterprise

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Build-time enterprise relays / adapter URL validation and canonicalization (`wss://host/`, IDNA, HTTPS or loopback adapter) | src-tauri/build.rs:170-190; src-tauri/src/enterprise_auth_build.rs:10-42; enterprise_relay_url.rs:1-75; enterprise_adapter_url.rs:3-26 | MISSING | Build script plus validators | S |
| `enterprise_login_gate` (`GET /info` 128 KiB; Required only for federated client-attached offline-jwt; fails closed) | src-tauri/src/enterprise_login_gate.rs:35-121 | MISSING | buzz-app registers it but no TS calls it; docs/port/relay.md §1.11 marks optional | M |

### Profiling, debug, budget

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Relay profiler (2048-sample ring, stages, Server-Timing) and client metrics | profiling.ts:1-94; transport.ts:247-283, 1361-1381 | MISSING | Dev-only; tracing spans | S |
| Frame traffic log and `relayDebug` | live.ts:280-286, 689, 718; debug.ts:1-7 | PARTIAL | Tracing only for loss, NOTICE, bad signatures (socket.rs:282, 495, 514, 595) | S |
| `yieldToHost` batches of 12 | yield.ts:1-12; transport.ts:234-241 | DONE (reader.rs:544-547, `spawn_blocking`) | - | - |
| Budgets (32 KiB input, byteSize, ByteLru) | budget.ts:1-71 | DONE (outbox.rs:51, 749; unread.rs:319) | - | - |
| Dev browser broker transport and SSE live | transport.ts:356-1085; broker-live.ts:1-558 | DIFFERENT | Deliberately not ported (no browser host) | - |

## Session, caching, timeline windows and folding

Port fold engine (rows, ordering, incremental projection, attachments) matches buzz-app. The biggest gaps are persistence and caching: none of the disk-backed startup behaviour is wired up (cached heads, startup roster cache, offline resume). Paths: buzz-app `src/features/relay/`, port `crates/relay/src/`, unless prefixed.

### Session lifecycle and event pipeline

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| One session per community+viewer; dispose stops all work and writes | session.ts:203-218, 2650-2694 | DONE (session/mod.rs:174-263, 627-686) | - | - |
| Live events go through one ordered pipeline (discovery, read state, unread, outbox echo, profiles, emoji, timelines) | session.ts:517-578 | DONE (session/mod.rs:1150-1204) | - | - |
| Finite reads (head and older pages) go through the same pipeline as live events | session.ts:417-516 | PARTIAL | Page results only reach `reads_accept` (session/mod.rs:759-771); outbox echo, profiles and emoji never see page events | S |
| Visibility gate for auxiliary events (5/7/9005/40003/39005 need a known visible target); re-filter retained events on access change | event-access.ts:7-43; store.ts:2256-2335 | PARTIAL | Port only drops timelines of denied channels (session/mod.rs:999-1021); no target-evidence check, no re-filter | M |
| 401/403 on a channel read denies that channel | store.ts:476-511, 662-669 | DONE (session/mod.rs:1333-1341) | Disk-head removal comes with persistence | - |
| One send clock per session shared by outbox and projections | message-order.ts:21-58; store.ts:146 | DONE (session/mod.rs:607-609, 857-866) | - | - |

### Finite reader

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Shared identical reads, per-consumer cancel, 3 slots / 1 background, 10 s deadline, 128 pending, 1–4 filters, limit 1–500, 64 KiB key, 8 MiB response | reader.ts:46-299 | DONE (reader.rs:22-26, 131-560) | - | - |
| `invalidate()` on disconnect, access revocation, cache clear and before catch-up; `promote()` for selected channel catch-up | reader.ts:311-327; session.ts:371-380, 2253, 2291, 2484 | MISSING | `Reader::invalidate`/`promote` exist (reader.rs:313-323) but nothing calls them | S |
| Strict event checks, signature verification, 2048-entry proof memo, one bad event rejects the batch | events.ts:299-380 | DONE (event.rs:205-290) | - | - |

### Timeline windows: head and older pages

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Page filter: kinds 9/40002/40008/40099, `top_level`, `include_aux`, `include_summaries`, limit 20, `until`/`before_id` | window.ts:7-24 | DONE (window.rs:12, 33-50) | - | - |
| Signed 39006 bound checks | window.ts:27-80 | DONE (window.rs:53-124) | - | - |
| Head page replaces retained pages, older page appends; `hasMore = has_more && cursor` | store.ts:610-661 | DONE (session/timeline.rs:139-173) | - | - |
| Older page over budget (2400 rows / 8 MiB) discarded and sets `historyLimited` | store.ts:638-648 | PARTIAL | Port inserts the page before checking (timeline.rs:154-167) so over-budget events stay in memory; check before inserting | S |
| Load-older guard | store.ts:2054-2072 | DONE (session/mod.rs:1259-1273) | - | - |
| Errors: first head fails = error; failure with rows keeps rows; "Retry older messages" | store.ts:662-680; ChannelTimeline.tsx:740-758 | DONE (timeline.rs:299-334; crates/app/src/ui/channel.rs:553-575) | - | - |
| Head newer than 60 s (`FRESH_FOR`) reused without a read | store.ts:99, 1981-1992 | DONE (timeline.rs:16, 133-136) | - | - |
| Revisit revalidates only while the reader is at the head; a paged-back reader is never replaced | store.ts:1966-1978 | DIFFERENT | `needs_head` ignores `at_head` (timeline.rs:133-136); reopening after 60 s or a disconnect wipes older pages and cursor (session/mod.rs:1219-1221) | S |
| Session-type channels: flat paging (20 rows, `#e` overlay paging 500/page up to 2000 events / 4 MiB, keep paging until empty) and replies kept as rows | session-window.ts:26-92; store.ts:117-122, 512-536 | MISSING | Every channel uses `page_filter`/`parse_page` (session/mod.rs:1275-1295) and `include_replies` is never set; session channels likely fail with "missing bound" | M |
| Prepared head cache (64 heads / 4 MiB) and hover/intent prepare | store.ts:179, 537-609, 1994-2040 | MISSING | Head cache plus `prepare(channel)` from the sidebar | M |
| Window LRU of 3; mounted readers never evicted | store.ts:137, 356-364 | DONE (session/mod.rs:63, 1226-1242) | - | - |
| Eviction fences in-flight reads | store.ts:348-355, 398-402 | PARTIAL | Recreated `Timeline` restarts generation at 0 (timeline.rs:108); use a session-wide counter | S |

### Live merge and window limits

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Live kinds accepted into an open window incl. transitive references | store.ts:2170-2199 | DONE (timeline.rs:18-29, 193-225) | - | - |
| Live events never move cursors | store.ts:2134 | DONE (timeline.rs:193-225) | - | - |
| Live overflow trimming (newest 2400 rows + local ops, shed oldest quarter under 8 MiB, set `historyLimited`) | store.ts:2204-2240 | MISSING | `traffic` grows without bound in a long-open channel (timeline.rs:193-225) | S |
| Local outbox ops shown in the window; failed edits/reactions hidden, failed content kept | store.ts:267-303, 2080-2133 | PARTIAL | Rule matches (timeline.rs:229-252) but `open()` never seeds local ops (session/mod.rs:1208-1224); a newly opened channel shows no pending/failed sends until the outbox changes | S |
| Tails: newest 256 live events per unopened channel (64 channels / 4 MiB), seeding windows and channel preview | store.ts:181-184, 215-266, 2145-2169 | MISSING | Tails plus `preview` on `ChannelSummary` | S |
| Row profile demand incl. membership actor/target, workflow owner, mention references | membership.ts:39-49; store.ts:426-440 | PARTIAL | timeline.rs:304-316 omits those ids | S |

### Gap fill after reconnect

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| On disconnect mark heads/windows cached and cancel pending reads | store.ts:2384-2389; session.ts:2461-2486 | PARTIAL | `mark_stale` works (session/mod.rs:1122-1131); reads are not cancelled | S |
| Catch-up head per retained window after route established, merged not replaced | session.ts:2527-2551; store.ts:2392-2432 | DONE (session/mod.rs:1084-1100; timeline.rs:177-188) | - | - |
| Pre-establishment head read cancelled so catch-up is a fresh post-subscribe read | session.ts:2228-2237, 2284-2292 | PARTIAL | `load_head` returns early while `loading_head` (session/mod.rs:1251-1253); the stale read clears `stale` | S |
| Catch-up scheduling: max 2 concurrent, selected first, promotion, Retry-After pacing | session.ts:2238-2318 | PARTIAL | All stale timelines read at once (session/mod.rs:1275-1295); no Retry-After | S |
| Per-channel catch-up state shown in "Live updates need attention" toast with Retry | session.ts:893-950; src/bundled/channels/LiveStatus.tsx:6-98 | MISSING | Failed catch-up lands in `TimelineSnapshot.error` shown only as "Retry older messages" (crates/app/src/ui/channel.rs:563) | M |
| Older pages load over HTTP while socket is down | ChannelTimeline.tsx:637-646 | DONE (session/mod.rs:1259-1273) | - | - |

### Folding and ordering

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Row selection (row kinds, `h`, replies only with `broadcast=1`) | fold.ts:285-296 | DONE (fold/mod.rs:318-332) | - | - |
| Delete rules; deleted rows dropped | fold.ts:221-243, 296 | DONE (fold/mod.rs:203-224) | - | - |
| Edits by author/displayed author, latest wins, 40002 unwrap, `edited` | fold.ts:318-328 | DONE (fold/mod.rs:342-360) | - | - |
| Attachments/emoji from newest edit, mentions from original, `attachmentSourceId` | fold.ts:329-419 | DONE (fold/mod.rs:362-425) | - | - |
| Reaction grouping by (content, emoji URL), first-reaction order | fold.ts:432-487 | DONE (fold/mod.rs:470-517) | - | - |
| Thread summary 39005 tie-break | fold.ts:167-197, 261-266 | DIFFERENT | Same-second tie: larger count wins before lower id (fold/mod.rs:625-632); intentional per spec §9.3 #14 | S |
| 40099 membership rows | membership.ts:12-37; fold.ts:297-317 | DONE (fold/membership.rs:26-52) | - | - |
| Displayed author (`actor` / leading `p`), signer, workflow owner | fold.ts:199-219 | DONE (fold/mod.rs:175-195, 551-573) | - | - |
| Diff metadata, sent-from-thread excerpt, agent envelope, `mentionReferences` | fold.ts:352-411 | DONE (fold/mod.rs:401-440) | - | - |
| Rendered order (effective ms then id; `ms` tag) | message-order.ts:7-25 | DONE (fold/order.rs:19-43) | - | - |
| Send clock (5 s lead others, 60 s own) | message-order.ts:27-58 | DONE (fold/order.rs:47-95) | - | - |
| Incremental refold | message-projection.ts:13-169 | DONE (fold/projection.rs) | - | - |
| Unchanged rows keep identity | row-identity.ts:1-102 | DONE (fold/projection.rs) | - | - |
| `newer()` tie-break | events.ts:141-150 | DONE (event.rs:199-202) | - | - |

### Message content parsing

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| `imeta` parsing (dim, blurhash, size, duration, waveform, voice note, filename, hash) | fold.ts:25-165 | DONE (fold/attachments.rs:93-176) | - | - |
| Attachments HTTPS-only | message-content.ts:21-24; fold.ts:98-99 | DIFFERENT | HTTP URLs kept as an "insecure" unavailable file card (fold/attachments.rs:81-88, 115-122, 160-163); looks deliberate | S |
| Markdown attachment projection | message-content.ts:113-223 | DONE (fold/attachments.rs:418-462, 593+) | - | - |
| Credential-free http(s) links; 100k length, depth 100 | message-content.ts:3-17, 67-99 | DONE (fold/attachments.rs:315-327, 465-480) | - | - |

### Message writes (`messages.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Send and reply tags (`h`, ≤32 `p`, `mention`, `emoji`, `imeta`, NIP-10) | messages.ts:75-99, 179-207 | DONE (messages.rs:462-517; session/mod.rs:1371-1406) | - | - |
| Every write kind requires "can participate" | messages.ts:42-48 | PARTIAL | Only Send checks it (session/mod.rs:1378-1384); Edit/React/Remove do not | S |
| Edit copies `imeta` from folded `attachmentSourceId` | messages.ts:209-244 | PARTIAL | session/mod.rs:1408-1417 passes the original event; pass the row's `attachment_source_id` | S |
| React (custom emoji tag only for `:code:`) | messages.ts:245-264 | DONE (messages.rs:724-760) | - | - |
| Remove own messages/reactions with `k`; agent owner removes agent's messages | messages.ts:265-314 | PARTIAL | Agent-owner case gets `None` (session/mod.rs:1438) | S |
| Report (1984, NIP-56) | messages.ts:316-335 | PARTIAL | Builder exists (messages.rs:887); no session/UI caller | S |
| Send thread reply to channel (sent-from-thread copy) | messages.ts:101-178 | PARTIAL | Builder exists (messages.rs:584); no session/UI caller | S |

### Persistence, caching and partitions

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Partition key `origin:viewer` | partition.ts:1-33 | DONE (store.rs:12-27) | - | - |
| Leaving purges heads, read state and outbox for that partition | partition-purge.ts:9-28 | DONE (store.rs:103-111, 397-410; crates/app/src/community.rs:211-240) | - | - |
| Disk head cache (signed events + author profiles; 64 heads / 8 MiB; 24 h; sessions excluded) | persistence.ts:31-204; store.ts:450-475, 596-606 | MISSING | `save_head`/`load_head`/`trim_heads` exist (store.rs:284-395), never called | L |
| Startup restore of cached heads (last-selected first, re-verify, shown cached, revalidate) | store.ts:685-822, 1936-1993 | MISSING | Load at session open, mark stale, revalidate | M |
| Disk-head retention follows roster | store.ts:950-962, 2332 | MISSING | Depends on the head cache | S |
| Startup roster cache (≤2048 rosters + ≤1024 profiles, 24 h, author must match) | store.ts:823-903 | MISSING | `startup_cache` table unused; discovery already supports `cached` (discovery.rs:315, 529, 680) | M |
| Offline resume: cached-only session shows saved sidebar and heads ("Reconnect to update saved conversations.") | src/features/relay/service.ts:85-130; session.ts:417-431 | MISSING | Port shows Connecting with an empty roster until NIP-11 succeeds | L |
| Clear cache (Developer settings) and wipe disk heads when connection denied | session.ts:2604-2648; store.ts:1637-1659; service.ts:262-330 | MISSING | Session `clear_cache` plus disk wipe | M |

### Other

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Incoming live messages for notifications (first sight, single `h`, 4096-char preview) | incoming.ts:1-19; session.ts:2324-2448 | DONE (session/incoming.rs:1-203) | - | - |
| Delivery receipt read (16 KiB cap, strict UTF-8) | receipt.ts:1-21 | DONE (outbox.rs:53, 270-282) | - | - |
| Generic filtered views for plugins (`projectEvents`, `observe`, `subscribeLive`) | projection.ts:1-104; session.ts:1616-1622, 1924+ | MISSING | Only if plugin-style views are ported | M |
| Preload avatar media for the channel about to open (last 12 authors) | store.ts:403-425 | MISSING | Performance only | S |

## Outbox, publishing, attachments and media

The outbox engine is a close port. Main gaps: the outbox UI, unsigned sends not shown in timelines, the outbox only existing after connect, ffmpeg media preparation, and streaming playback. Paths: buzz-app `src/features/relay/`, `src/bundled/channels/` or `src-tauri/src/relay/`; port `crates/relay/src/` or `crates/app/src/ui/`.

Key findings:
1. Unsigned sends never show in the timeline: only signed ops reach timelines (session/mod.rs:1462-1468, `op.signed.clone()?`). A send queued behind 3 deliveries, failed before signing, or restored unsigned never appears and, with no outbox panel, cannot be retried or dismissed.
2. The outbox is created only after NIP-11 succeeds (session/mod.rs:846-888); before that every write fails with "Not connected to the relay yet" and restored ops are not shown (buzz-app creates it with the session, session.ts:264-317).
3. No outbox UI, and failed edits/deletes/reactions are left out of the timeline (session/timeline.rs:246), so they vanish silently.

### Outbox core

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Delivery states and record shape (recovery, acknowledged, guarded) | outbox.ts:17-43 | DONE (outbox.rs:81-133) | - | - |
| Event built at send time: `client-id` UUID, `ms` tag, local id | outbox.ts:679-744 | DONE (outbox.rs:775-827) | - | - |
| Send checks and texts (kind allowed, kind 9 non-empty, 32 KiB, MAX_PENDING 256, unique recovery key) | outbox.ts:647-678 | DONE (outbox.rs:724-757, 809-826) | - | - |
| Edit (40003) created_at +1 s per target, 60 s lead cap | outbox.ts:686-709 | DONE (outbox.rs:770-803) | - | - |
| NIP-AP catalog kinds: `supersedes`, head+1, larger limits | outbox.ts:665-674, 710-727 | MISSING | `Draft` has no `supersedes` (messages.rs:31-35); add if catalog publishing is ported | S |
| Unsigned intent saved before signing; signed bytes before publishing | outbox.ts:442-468 | DONE (outbox.rs:1358-1411) | - | - |
| ≤3 concurrent deliveries; 10 s deadline from enqueue | outbox.ts:90, 407-428 | DONE (outbox.rs:54-55, 1262-1337) | - | - |
| Failure classes, no downgrade, "Retry blocked/failed:" prefixes, timeout text | outbox.ts:381-406, 500-532 | DONE (outbox.rs:1472-1526) | - | - |
| acceptPublish status mapping, 16 KiB receipt | transport.ts:1313-1359 | DONE (outbox.rs:239-296) | - | - |
| Quota refusal (`sent:false` + `rate-limited:` or `quota:"api"` = rejected) | transport.ts:1326-1339 | PARTIAL | Only 429 + "rate-limited: quota exceeded" recognised (outbox.rs:256-265; http.rs:136-149); read `sent`/`quota` fields | S |
| `accepted:false` with "duplicate:" | transport.ts:1352-1357 (failed) | DIFFERENT | Counted as accepted (outbox.rs:292), on purpose per spec §9.3 #3 | - |
| Event older than 15 min: strong read by id instead of re-post | native.ts:835-858 | DONE (outbox.rs:298-314) | - | - |
| Echo polling at 0/500/1500/4000/10000 ms | session.ts:579-602; outbox.ts:531-532 | DONE (outbox.rs:56-62, 1562-1607) | - | - |
| `observe()` marks seen on verified echo | outbox.ts:838-892 | DONE (outbox.rs:1609-1651; session/mod.rs:1186-1187) | - | - |
| Retry reuses saved signature; refusal rules | outbox.ts:767-794 | DONE (outbox.rs:849-886) | - | - |
| dismiss / recover / acknowledge rules and texts | outbox.ts:587-636, 795-815, 117-122 | DONE (outbox.rs:891-1000, 1339-1353) | - | - |
| Workflow receipt callback | session.ts:293-294 | PARTIAL | Session passes `on_receipt: None` (session/mod.rs:878-886) | S |
| preparePublish: fresh mention roster check, canvas/workflow validation, re-check at publish start | session.ts:296-315 | PARTIAL | `Publisher::prepare` not overridden (outbox.rs:333-335); mentions checked once against cached discovery (session/mod.rs:1385-1391) | M |
| `observeSend` hook (mention-wake, unhide DM on outgoing send) | outbox.ts:48-50, 153-177 | MISSING | Hook and consumers | S |
| Serialized journal commits; confirmed LRU 2048 / 8 MiB; "Outbox storage is full" over 2 MiB | outbox.ts:205-276, 127 | DONE (outbox.rs:45-49, 1065-1153; store.rs:136, 512-660) | - | - |
| Hydration budgets, batches, checks, no automatic replay, load error blocks sends | outbox.ts:292-379 | DONE (outbox.rs:1170-1258) | - | - |
| Migration from localStorage / IndexedDB v1 journal | outbox-storage.ts:57-95 | DIFFERENT | Not needed (no earlier native journal) | - |
| purgeConfirmed on access revocation; dispose | outbox.ts:826-837, 893-913 | DONE (outbox.rs:1011-1036) | - | - |
| Journal deleted on leave | outbox-storage.ts:27-49 | DONE (store.rs:397-409; crates/app/src/community.rs:237) | - | - |
| Outbox exists from session start (offline queueing, restored ops visible) | session.ts:264-317 | PARTIAL | Created only after NIP-11 (session/mod.rs:846-888); writes fail "Not connected to the relay yet" (1353-1356) | M |
| Optimistic row shown at once with `sending` | outbox.ts:747-758 | PARTIAL | Only signed ops reach timelines (session/mod.rs:1462-1468) | M |

### Outbox and delivery UI

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Outbox panel ("Outbox · N items", Sending/Sent/Unconfirmed/Not sent, 100-char excerpt, "Add agent …", note, RelayTimings) | src/bundled/channels/OutboxStatus.tsx:10-50, 98-99; ChannelsPage.tsx:1196-1200 | MISSING | Build the panel | M |
| Panel Retry (hidden for workflow/guarded) and guarded hint | OutboxStatus.tsx:52-69 | MISSING | Part of panel | S |
| "Remove from outbox" with error alert | OutboxStatus.tsx:69-97 | MISSING | `Session::dismiss` exists (session/mod.rs:423), no UI | S |
| Message delivery notice: 10 s grace, "Couldn't send this message." / "Delivery not yet confirmed.", Retry | src/features/messages/delivery.ts:3-18 | PARTIAL | Only failed/unknown, shown immediately (crates/app/src/ui/message.rs:315-318, 910-946); retry errors ignored (crates/app/src/ui/channel.rs:503-507) | S |
| Edit/delete/reaction failure UI ("Retry message update", "Discard failed update", "Couldn't update reaction.", "Retry reaction") | MessageManagement.tsx:484-551; ReactionTool.tsx:145-166 | MISSING | Failed non-row ops hidden (session/timeline.rs:246); add status rows | M |
| No edit/delete of unconfirmed rows | useMessageEdit.ts:13-38 | DONE (crates/app/src/ui/message.rs:462) | - | - |
| useComposerSent: reveal sent message; re-select exact-message session channel | src/bundled/channels/useComposerSent.ts:4-19; ChannelsPage.tsx:545-550 | PARTIAL | Tail follow only (crates/app/src/ui/channel.rs:326-329) | S |

### Channel writes (src-tauri/src/relay/channel_writes.rs)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Route-checked 9002/9008/9021/9022/41012 shapes | channel_writes.rs:21-80, 118-177 | DIFFERENT | No untrusted renderer to guard; builders in lifecycle.rs:33-34, 490-513, 607-620 | - |

### Attachments, limits, imeta

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Per-type caps (GIF 10 / image 50 / video 500 / other 100 MiB), error text | attachment-limits.ts:1-10; attachments.ts:30-32 | DONE (messages.rs:277-287; upload.rs:20, 29-32; crates/app/src/ui/composer.rs:1062) | - | - |
| ≤10 files per message | src/features/messages/attachment-draft.ts:34, 189-191 | DONE (crates/app/src/ui/composer.rs:40, 1052-1055) | - | - |
| 1,000 MiB retained-draft cap | attachment-draft.ts:35, 196-205 | MISSING | Add the check | S |
| Upload result validation (MIME, hex, size, same-origin `/media/<sha>`); image type sniffing | attachments.ts:55-97; attachment-limits.ts:12-21 | DONE (upload.rs:60-72, 345-361; messages.rs:295-333) | - | - |
| Upload response mapping (401/403, 413, 429, 8192-byte cap, JSON `code`) | attachments.ts:220-281 | PARTIAL | Codes `unavailable`, `ffmpeg`, `io`, `video`, `cancelled` fall back to Failed (upload.rs:27-49, 329-389) | S |
| imeta field order, filename cleaning, label escaping, inline types | attachments.ts:98-106, 284-320 | DONE (messages.rs:348-420) | - | - |
| Voice imeta `duration` + `waveform` | attachments.ts:312, 322-334 | DIFFERENT | Duration only (messages.rs:271-273, 403-408), deliberate per spec §9.3 #2 | - |
| Reading blurhash, `dim`, `thumb`/`image`, waveform; blurhash preview | blurhash.ts:1-15 | DONE (fold/attachments.rs:131-152, 273-312; crates/app/src/ui/media.rs:151-170) | - | - |

### Upload pipeline

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Blossom 24242 auth (t=upload, x=sha, server, +60 s, "Upload attachment") | src-tauri/src/relay.rs:820-855 | DONE (crates/identity/src/lib.rs:261-284; media.rs:215-221) | - | - |
| PUT /upload with Content-Type, X-SHA-256, 600 s timeout | relay.rs:1145-1189 | DONE (upload.rs:307-327; media.rs:66-71) | - | - |
| Body streamed from disk in 64 KiB chunks | relay.rs:1105-1143 | DIFFERENT | Whole file in RAM and cloned (crates/app/src/ui/composer.rs:1012-1030, 1116-1121; up to 500 MiB × 10); stream from path | M |
| Upload progress shown on the draft chip | relay.rs:1124-1129; attachment-draft.ts:128-140 | MISSING | Spinner only (composer.rs:1543-1554) | M |
| Upload timing: queue on attach, upload serially at Send, Send stays enabled | attachment-draft.ts:257-266 | DIFFERENT | Uploads start on attach, Send disabled until done, removed files still upload (composer.rs:400-411, 1083-1141) | M |
| Removing a file aborts its upload; leaving pauses ("Upload paused. Retry to continue.") | attachment-draft.ts:70-84, 267-271; relay.rs:857-926 | MISSING | Upload keeps running (composer.rs:1269-1282) | S |
| Spool temp dirs, 4 spool slots, 64 active uploads, "Uploads are busy" | upload_spool.rs:14-15, 149-339 | DIFFERENT | No IPC boundary; uploads serial per composer | S |
| Retry restarts from scratch | attachment-draft.ts:272-276 | DONE (composer.rs:1284-1292) | - | - |
| Image metadata stripping (JPEG q0.95 + orientation, PNG re-encode, WebP orientation, 25 MP cap) | prepare-attachment.ts:25-101 | PARTIAL | PNG chunk-cleaned not re-encoded, WebP orientation not applied, error text differs (upload.rs:109-270) | S |

### Media preparation

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| HEIC/HEIF to JPEG via ffmpeg ≥ 8 | video-preparation.ts:54-67; media_preparation.rs:99-141, 245-300 | MISSING | HEIC uploads raw (upload.rs:6-8, 98-99) | L |
| Video to MP4 (libx264 crf 23, yuv420p, aac 128k, faststart, metadata stripped; MP4 too) | video-preparation.ts:4-51; attachments.ts:113-121; media_preparation.rs:54-79, 300-345 | MISSING | Raw upload with extension MIME (upload.rs:84-88) | L |
| Preparation limits (2 concurrent, 60 s / 600 s, 50/500 MiB output, ffmpeg texts) | media_preparation.rs:11-49, 153-189 | MISSING | Comes with the above | S |
| Voice note MP4 with black 16x16 H.264 + AAC, 24 kHz mono, ≤300 s | media_preparation.rs:65-72, 276-286; voice-media.ts:2-3 | DIFFERENT | Same output via GStreamer (crates/app/src/ui/media/recorder.rs:1-45, 244-350) | - |
| Recording validation; picked `voice-note-*` audio converted | voice-media.ts:9-24; video-preparation.ts:69-93 | PARTIAL | Picked voice-note file uploads raw (recorder.rs:24-30) | S |

### Media download and streaming

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Signed media GET, same-origin path check, no redirects, `.thumb.jpg` for avatars | relay.rs:1641-1717 | DONE (media.rs:98-105, 126-211, 223-244) | - | - |
| Ranged reads through 1 MiB block cache (32 blocks) | media_blocks.rs:18-174 | MISSING | Whole body before decode (media.rs:126-153) | L |
| Non-Range origins: video spooled to disk up to 500 MiB | media_spool.rs:1-47, 152-293 | MISSING | Whole-body cap 100 MiB (media.rs:22): videos over 100 MiB fail "The media is too large" | M |
| Loopback HTTP server for media elements | media_stream.rs:1-200 | DIFFERENT | Not needed natively (GStreamer appsrc) | - |
| Response type policy and cache headers | relay.rs:1729-1809 | DIFFERENT | In-memory LRU 64 MiB (media.rs:26-27, 246-287) | S |
| Avatar prefetch/warm | media.ts:37-143 | MISSING | Optional | S |
| media_download to Downloads, errors returned | relay.rs:1579-1601 | DIFFERENT | Also opens file; failures only logged (crates/app/src/ui/media.rs:375-400) | S |
| Copy image to clipboard in viewer | relay.rs:1553-1575; ImageReviewStage.tsx:140-150, 392 | MISSING | Add "Copy image" (crates/app/src/ui/media/review.rs:390-397) | S |

## Read state and unread

The data layer is ported almost one to one. The gaps are in the session API and the UI. Prefixes: buzz-app `src/`, port `crates/relay/src/` or `crates/app/src/`.

### Wire format and codec

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Read-state event recognition (30078, one `d`, one `t=read-state`, 32-hex slot) and target-key validation | features/relay/read-state-model.ts:26-33, 59-74 | DONE (read_state.rs:197-210, 270-289) | - | - |
| v1 payload parsing (unknown `v` fails closed, client_id 1..64, ≤10k keys / 128 KiB, `ov_s/c/b` groups, `esc:` unescape) | read-state-model.ts:43-133 | DONE (read_state.rs:294-364) | - | - |
| Encode for publish (sorted/escaped keys, override registers, re-parse, ≤40 KiB) | read-state-model.ts:225-258 | DONE (read_state.rs:474-505) | - | - |
| Codec: NIP-44 v2 to self, tags exactly `[d,t]`, ±60 s skew, 96 KiB / 128 KiB decode limits | read-state-host.ts:23-35; read-state.ts:291-318 | DONE (read_state.rs:523-578) | - | - |
| Post-sign decode confirms payload, coordinate, created_at | read-state.ts:555-567 | DIFFERENT | Only a `debug_assert` (read_state.rs:2334-2337); optional runtime check | S |
| Event ≤64 KiB and decode re-check before `POST /events` | read-state-model.ts:6; src-tauri/src/relay.rs:548-562 | PARTIAL | `READ_STATE_EVENT_BYTES` unused (read_state.rs:38); run_publish posts without a check (session/unread.rs:495-506) | S |
| Timestamp refusal classification (exact error + 400, or 503 `sent:false`) | read-state-host.ts:4-15; transport.ts:1289 | PARTIAL | `publish_outcome` passes only `{"error":msg}` (session/unread.rs:195-202), so 503 `sent:false` never matches (read_state.rs:1469-1481) | S |

### Merge rules

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Max-per-key merge, 10k combined cap, `effectiveFrontier`, `overrideActive` | read-state-model.ts:144-190 | DONE (read_state.rs:368-417) | - | - |
| Publishing with overrides present | read-state.ts:531-533 (blocked: "Synchronized override publication is not enabled") | DIFFERENT | Port strips overrides and publishes frontiers (read_state.rs:14-22, 2313-2326); confirm intended | S |

### Local journal

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Journal schema and validation; corrupt journal never overwritten; homes pruned; local = reserve ∪ sync | read-state-storage.ts:19-196; read-state.ts:84-91 | DONE (read_state.rs:1036-1216, 1564-1615) | - | - |
| Durable storage per partition; cross-window coordination | read-state-storage.ts:197-281; read-state.ts:56-66, 156-159, 253-263 | DONE (SQLite `read_state`, store.rs:745-797; single actor) | - | - |
| Purge partition on leave | read-state-storage.ts:213-230 | DONE (crates/app/src/community.rs:203-238; store.rs:397-410) | - | - |

### Retention and compaction

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| `retainRead` (96 KiB, protected floors, 75% scoped share, recency, coverage pruning) | read-state-retention.ts:51-229 | DONE (read_state.rs:763-921) | - | - |
| `retainLocalRead` (5,000 keys / 512 KiB reserve) | read-state-retention.ts:244-325 | DONE (read_state.rs:936-1019) | - | - |
| Second retention at publish to 40 KiB | read-state.ts:534-540 | DONE (read_state.rs:2315-2322) | - | - |
| Coverage rules and `findHomes` (batches of 100, once per session) | unread.ts:590-624, 1557-1632 | DONE (unread.rs:976-1012, 2252-2316; session/unread.rs:649-676) | - | - |

### Sync lifecycle

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Capability/status/completeness snapshot; NIP-11 complete snapshot or bounded 500 read | read-state.ts:33-46, 146-155, 356-409; read-state-snapshot.ts:9-77 | DONE (read_state.rs:614-687, 1263-1300, 1857-1918; session/unread.rs:434-471) | - | - |
| Snapshot response capped at 8 MiB + 4 KiB | read-state-snapshot.ts:81-100 | PARTIAL | Only the generic 8 MiB cap (http.rs:11); a maximal snapshot fails TooLarge | S |
| Read priorities: foreground on first ensure, background on reconnect | read-state.ts:702-703; unread.ts:1492, 2312-2314 | DIFFERENT | Marker refresh always default, repair always background (session/unread.rs:460, 616) | S |
| Live read-state route, ingest newest per coordinate, 4096 slot cap | read-state.ts:319-355, 817-839; live.ts:600-608 | DONE (live.rs:493; session/mod.rs:767, 1185; session/unread.rs:386-395; read_state.rs:1769-1852) | - | - |
| Mutations with fences, revision, recent, localUnread | read-state.ts:410-490, 722-814 | DONE (read_state.rs:1922-2041) | - | - |
| 5000 ms non-resetting debounce; failed revision waits for retry | read-state.ts:113, 264-290, 575-590 | DONE (read_state.rs:51, 1727-1765, 2115-2130; session/unread.rs:550-585) | - | - |
| Flush: retry saved pending first, fresh own-slot read, slot conflict, save signed bytes before send, 10 s timeouts | read-state.ts:491-592 | DONE (read_state.rs:509-515, 2075-2349; session/unread.rs:43, 495-521) | - | - |
| `publishPending` readback and once-per-flush timestamp renewal | read-state.ts:593-670 | DONE (read_state.rs:2133-2288) | - | - |
| `stale()` on disconnect; refresh then repair on reconnect | read-state.ts:840-843; unread.ts:2312-2321 | DONE (session/unread.rs:296-326) | - | - |
| `retrySync()` / `refresh()` user actions | unread.ts:1838-1847 | PARTIAL | `retry_reads` only via generic `Session::retry` (session/mod.rs:717-729); no dedicated control | S |

### Unread computation

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Evidence store (kinds, 4096 / 8 MiB, overflow clears and marks stale, depth-32 ownership) | unread.ts:153-186, 261-269, 1665-1706 | DONE (unread.rs:273-315, 485-488, 2019-2062) | - | - |
| Index: tombstones, own/joined/ownThreads, root walk | unread.ts:390-455 | DONE (unread.rs:1182-1258) | - | - |
| `mentionsViewer`, relevance/category/priority predicates, follows | unread.ts:372-389, 487-561, 625-642 | DONE (unread.rs:1140-1154, 1267-1356) | - | - |
| `isUnread` / `afterFrontier` rules | unread.ts:513-556 | DONE (unread.rs:1360-1407) | - | - |
| Per-target snapshot (null counts without evidence, latestMessage, manual, freshness) | unread.ts:693-766 | DONE (unread.rs:1476-1574) | - | - |
| Snapshot selector cache of 4096 | unread.ts:886-903 | DIFFERENT | Clears whole cache when full (unread.rs:1598-1600); harmless | S |
| DM vs mention classification ("direct wins in DMs") | unread.ts:625-639 | DIFFERENT (bug) | `mentions` counts DM entries (unread.rs:1536-1538): DM rows get a numeric pill and tray `mentions + direct` double-counts DMs (crates/app/src/main_window.rs:277-285) | S |
| Thread-activity projection (unread replies by root, preview with edits) | unread.ts:775-854 | PARTIAL | Engine ported (unread.rs:1688-1796) but preview is raw content (unread.rs:1726) and `UnreadCounts` has no activity, so the session never exposes it (session/unread.rs:49-87) | M |
| Channel-list changes republish/purge/repair | unread.ts:1108-1209 | DONE (unread.rs:2067-2143) | - | - |
| Evidence repair (128 channels per read, limit 500, progressive) | unread.ts:1492-1556 | DONE (unread.rs:2190-2248; session/unread.rs:591-660) | - | - |
| Mute does not change unread | docs §5i.7 | DONE (unread.rs:17-20) | - | - |
| Explicit follow / unfollow | unread.ts:1849-1876 | PARTIAL | Engine done (unread.rs:2531-2567); no `Session` API | S |

### Lookups and attention

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Conversation-membership lookups (chunks of 50, reply pages, backoff `min(60 s, 1 s·2^n)`) | unread.ts:1218-1491, 1636-1664 | DONE (unread.rs:1410-1472, 1968-2013, 2326-2525; session/unread.rs:680-789) | - | - |
| `attention()` incl. `pending`; notifications use it | unread.ts:643-692 | DONE (unread.rs:1618-1686; session/incoming.rs:131, 175) | - | - |

### Automatic reading

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Reading lease (≤64, fenced) | unread.ts:1877-1899 | DONE (unread.rs:2962-3001) | - | - |
| `observe()` writes `msg:` marks; DM observation reads whole DM | unread.ts:1905-1938, 2021-2066 | DONE (unread.rs:3028-3069, 3163-3205) | - | - |
| Channel `catchUp()` | unread.ts:1956-2020 | DONE (unread.rs:3075-3159; session/unread.rs:905-928) | - | - |
| Thread-panel reading (`thread-activity:` catch-up, reply observations) | unread.ts:1958-1969; use-reading.ts | MISSING | `fire_dwell` only calls `catch_up(.., None)` (session/unread.rs:916-921); thread.rs reports no viewport | M |
| Dwell gating (fully visible rows, window focused, document visible, 300 ms, bottom within 1 px) | docs §5i.10; use-reading.ts:29-234 | PARTIAL | 300 ms matches (session/unread.rs:45) but partially visible rows count, only on scroll (crates/app/src/ui/channel.rs:164-179), no focus/visibility gating; new channel assumed at bottom (session/unread.rs:846-851) | M |

### Explicit read and unread actions

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| `markChannelRead` | unread.ts:1772-1805, 2172-2178 | DONE (unread.rs:2691-2764; session/mod.rs:440-442) | - | - |
| `markAllChannelsRead` | unread.ts:2179-2233 | DONE (unread.rs:2770-2820; session/mod.rs:445-447) | - | - |
| `markThrough(target, msg)` | unread.ts:2069-2083 | PARTIAL | Engine done (unread.rs:2656-2670); not in session `Mark` (session/unread.rs:90-95) | S |
| `markMessageRead` | unread.ts:2110-2146 | DONE (unread.rs:2824-2857; session/mod.rs:450-452) | - | - |
| `markMessageUnread` and message menu "Mark unread" | unread.ts:2084-2109 | DONE (unread.rs:2862-2881; crates/app/src/ui/message.rs:485-491; crates/app/src/ui/channel.rs:428-437) | - | - |
| Message menu toggles "Mark unread" / "Mark read" | src/features/messages/MessageManagement.tsx:252-253 | PARTIAL | Always "Mark unread"; failures only logged (channel.rs:432-434) | S |
| `markUnreadLocal` / `clearUnreadLocal` | unread.ts:2234-2267 | PARTIAL | Engine done (unread.rs:2902-2924); no session API | S |
| `enterChannel` / `leaveChannel` visit semantics | unread.ts:243-258, 1763-1770, 2147-2171 | DONE (unread.rs:2672-2689, 2927-2957; session/unread.rs:832-845, 915) | - | - |

### Unread UI

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Bold label for ordinary backlog | src/bundled/channels/UnreadBadge.tsx:50-56 | DONE (unread.rs:555-563; crates/app/src/ui/sidebar.rs:159-163) | - | - |
| Priority accent dot, no numerals | UnreadBadge.tsx:57-60, 80-87; docs/port/ui.md:967-969 | DIFFERENT | Numeric mention pill instead (crates/app/src/ui/sidebar.rs:168-184), which ui.md:969 rules out; replace with 6 px dot | S |
| Thread-activity dot "N unread threads" | UnreadBadge.tsx:57, 63-64, 88-95 | MISSING | Needs activity exposure | S |
| Accessible/tooltip labels ("…Not an exact total", "Marked unread on this device only", "; may be out of date") | UnreadBadge.tsx:61-64 | MISSING | Labels | S |
| Off-screen "N unread" edge pills with DM avatar stack; reveal expands section, centres and focuses row | src/bundled/channels/SidebarUnread.tsx:30-239 | MISSING | Implement | M |
| "Mark as Read" / "Mark as Unread (this device)" in sidebar and header menus, pending/"Saving…"/error | src/bundled/channels/ChannelReadMenuItem.tsx:24-56; useChannelReadAction.ts:1-32 | MISSING | Menu items + action state (needs `markUnreadLocal`) | M |
| "Mark all as read" in community rail menu with disabled reasons and toasts | src/features/communities/CommunityRailItem.tsx:280-315 | MISSING | Rail context menu (API exists) | S |
| Channel activity popover (hover 250/150 ms; "Working now" with timers; "Unread threads" rows with per-row "Mark as read"; stale note) | src/bundled/channels/ChannelActivityPopover.tsx:33-357 | MISSING | Needs `activity()` and `markThrough` in session | L |
| `UnreadOptions` panel (mark unread on device, mark read through loaded, sync status, Refresh, Retry read sync) | UnreadBadge.tsx:100-177 | MISSING | Channel settings panel | M |
| `channel-activity.ts` values for Recent sort (kinds incl. 45001/45003, limit:1 per channel in batches of 128) | src/features/relay/channel-activity.ts:1-145 | PARTIAL | Model ported (sidebar.rs:541-683), not wired; sidebar sorts by name; search "recent" uses sampled `unread.latest` (crates/app/src/ui/search.rs:541-548) | M |

## Sidebar, channel discovery, navigation and DMs

Port data layer for the sidebar (`crates/relay/src/sidebar.rs`) is a near-complete pure port of the preference registers, edits, decoding and section projection, but nothing outside its own tests calls it. The UI sidebar (`crates/app/src/ui/sidebar.rs:76-284`) is a fixed Channels / Forums / Direct messages list.

### Discovery and membership (data)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Accept only relay-signed 39000/39002, capacity 1024 with overflow revision | src/features/relay/discovery.ts:40-80 | DONE (crates/relay/src/discovery.rs:317-389) | - | - |
| Deny / suspend / resume read-only previews after CLOSED | src/features/relay/discovery.ts:81-105 | DONE (crates/relay/src/discovery.rs:444-478) | - | - |
| Complete roster read proves absence (`retain`), `denyAll`, `restrictToKnown` for restored snapshots | src/features/relay/discovery.ts:110-143 | DONE (crates/relay/src/discovery.rs:494-540) | - | - |
| canAccess / canParticipate / authorized / open public metadata rules | src/features/relay/discovery.ts:145-171 | DONE (crates/relay/src/discovery.rs:541-578) | - | - |
| ChannelSummary projection (readOnly, cached, visibility, members, hidden, private, type incl. session + parentChannelId, archived, DM participants) | src/features/relay/discovery.ts:200-270 | DONE (crates/relay/src/discovery.rs:635-679) | - | - |
| clearCached and savedEvents for the roster snapshot | src/features/relay/discovery.ts:271-286 | DONE (crates/relay/src/discovery.rs:680-706) | - | - |
| Joined channels sorted by name then id | src/features/relay/discovery.ts:287-295 | DONE (crates/relay/src/discovery.rs:707) | - | - |
| 40099 membership activity parsing (relay-authored only, member_left/joined/removed) | src/features/relay/membership.ts:13-37 | DONE (crates/relay/src/discovery.rs:997) | - | - |
| `useListedChannel` per-channel fact selector | src/features/relay/listed-channel.ts:9-31 | DONE (snapshot `channels` read directly, crates/app/src/ui/sidebar.rs:83) | - | - |

### Sidebar sections and preferences

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Section layout: Starred, personal groups, Channels, Forums, Direct messages; empty Starred/Forums hidden | src/bundled/channels/sidebar-sections.ts:13-117 | PARTIAL: pure function ported (crates/relay/src/sidebar.rs:98-200) but UI renders fixed Channels/Forums/DMs only (crates/app/src/ui/sidebar.rs:79-93) | Load preferences and render through `sidebar_sections` | M |
| Per-section sort (A–Z / Recent by last activity) | src/bundled/channels/sidebar-sections.ts:40-55; src/bundled/channels/SidebarSection.tsx:160-195 | MISSING in UI (sort logic at crates/relay/src/sidebar.rs:76-95; UI always sorts by name, crates/app/src/ui/sidebar.rs:90-93) | Section "More actions" menu with Sort submenu, write `channel-sort`, show sort errors | M |
| Read sidebar preferences (4 coordinates of kind 30078, foreground read, error prefixes "Preference query:" / "Preference decode:") | src/features/relay/sidebar-preferences.ts:199-238 | PARTIAL: filters and decoder exist (crates/relay/src/sidebar.rs:1767-1866), session never reads them | Add a session-owned preferences store with ensure/refresh | M |
| Preference projection and validation budgets (100 sections, 1000 assignments, 500 stars/mutes, 104 sort keys) | src/features/relay/sidebar-preferences.ts:78-197 | DONE (crates/relay/src/sidebar.rs:1478-1534) | - | - |
| LWW register metadata (`meta`), legacy import, tombstones, 128 KiB budget, device id | src/features/relay/sidebar-registers.ts:1-300 | DONE (crates/relay/src/sidebar.rs:860-1310) | - | - |
| Pure edits: section removal, assignment, create-and-assign, sort, star/mute toggle | src/features/relay/sidebar-edits.ts:1-153 | DONE (crates/relay/src/sidebar.rs:1590-1760) | - | - |
| Native writer: strong head read, sign, publish, readback confirm, conflict error | src/features/relay/native-sidebar.ts:40-298 | PARTIAL: `SidebarWrite::prepare`/`confirms` exist (crates/relay/src/sidebar.rs:2062-2208); no publish path | Session action to read head, sign 30078 (NIP-44 to self), publish, read back | M |
| Preferences store: serialized write queue, optimistic pending moves, failed-move retry/dismiss, sort failures, cached-state guards | src/features/relay/sidebar-preferences-store.ts:39-719 | MISSING | Port the store (pending/failed maps, generation, clear/dispose) | L |
| Star / unstar channel ("Move to… Starred") | src/features/channel-navigation/ChannelSidebar.tsx:583-592, 640-660 | MISSING | Row menu item + star write | M |
| Move channel/conversation to a group, "Create new…", "Remove from <group>" | src/features/channel-navigation/ChannelSidebar.tsx:600-720 | MISSING | Move submenu and assignment writes | M |
| Create new section dialog ("Create new section", "Section name") | src/bundled/channels/CreateSidebarSection.tsx:39-80 | MISSING | Dialog that calls create-and-assign | S |
| Remove section with confirm ("…will move back to Channels…") | src/bundled/channels/SidebarSection.tsx:212-260 | MISSING | Section menu item, confirm dialog, removal write | S |
| Collapse / expand section, persisted per scope | src/bundled/channels/SidebarSection.tsx:126-210; src/bundled/channels/useSidebarView.ts:15-47 | MISSING (caret drawn but inert, crates/app/src/ui/sidebar.rs:105) | Toggle state saved in view state | S |
| Drag and drop channels between sections | src/features/channel-navigation/ChannelSidebarDnd.tsx:1-117 | MISSING | Drag sources and drop targets per section key | M |
| Section group icons (emoji / custom emoji) | src/bundled/channels/SidebarGroupIcon.tsx:1-79 | MISSING | Render group icon in header and menus | S |
| Personal groups dialog ("Personal groups", "Group name", add/remove/save) | src/bundled/channels/ChannelGroupsDialog.tsx:13-237; src/features/relay/sidebar-personal-groups.ts:42-172 | MISSING (data layer: see Me preferences) | Dialog and removal/assignment flows with their Outbox-guidance error texts | M |
| Mute / Unmute with optimistic intent | src/features/channel-navigation/ChannelSidebar.tsx:728-744; src/bundled/channels/useOptimisticMute.ts:13-72 | MISSING | Row/header menu item, mute write, optimistic state, muted unread suppression input | M |
| Sidebar row context menu (right click, ContextMenu key, Shift+F10) | src/bundled/channels/ChannelSidebarItem.tsx:272-344; src/bundled/channels/useChannelRowMenu.ts:1-41 | MISSING | Context menu with move, mute, read, copy link, lifecycle, remove-from-Messages | M |
| Row menu "Copy link" (`buzz://channel/<encoded id>`, toast "Link copied." / "Couldn't copy the link.") | src/bundled/channels/ChannelCopyLinkMenuItem.tsx:5-24 | PARTIAL: only in header menu, no toast and no URI encoding (crates/app/src/ui/channel_menu.rs:157-170) | Add to row menu, encode id, show toasts | S |
| Row menu lifecycle actions | src/features/channel-navigation/ChannelSidebar.tsx:763-772 | PARTIAL: only in header menu (crates/app/src/ui/channel_menu.rs:180-255) | Reuse in row menu | S |
| Startup reveal latch (1500 ms budget, wait for prefs/names/activity/unread) and "updating" state | src/bundled/channels/useSidebarStartup.ts:11-87 | MISSING (port shows whatever is loaded; `startup_cache_usable` unused, crates/relay/src/sidebar.rs:856) | Latch the first reveal | S |
| Channel sidebar width resize (220–520, default 260, double-click reset, persisted) | src/bundled/channels/useSidebarView.ts:4-13; src/features/channel-navigation/ChannelSidebarResizeHandle.tsx:1-27 | MISSING (fixed 260 px, crates/app/src/ui/sidebar.rs:276) | Resize handle and persisted width | S |
| Scroll position restore and reveal current row | src/bundled/channels/useSidebarView.ts:54-228 | MISSING | Save scroll per scope; scroll selected row into view | S |
| Sidebar connection states: "Connecting to your relay…", error + "Retry channels", "Choose a community to see channels.", "No channels yet.", "Retry connection", error boundary "Channels couldn't open." | src/features/channel-navigation/ChannelSidebar.tsx:100-185, 1229-1236 | MISSING (sidebar is just empty, crates/app/src/ui/workspace.rs:476-516) | Render status and retry | S |
| "Saving…" status and move error notes | src/features/channel-navigation/ChannelSidebar.tsx:1195-1210 | MISSING | Comes with the preferences store | S |

### Sidebar rows

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Active filter: not archived, hidden only for DMs; DMs hidden by viewer excluded | src/bundled/channels/sidebar-sections.ts:18-28 | DONE (crates/app/src/ui/sidebar.rs:52-54, 83) | - | - |
| Channel icon: lock for private, hash otherwise; DM icon | src/features/channels/channel-icon.ts:4-8 | DONE (crates/app/src/ui/message.rs:1035-1042) | - | - |
| DM label from participant names, "Notes to self", label profile demand capped at 1024 | src/bundled/channels/useChannelLabels.ts:9-77 | DONE (crates/app/src/ui/sidebar.rs:37-50; crates/relay/src/session/mod.rs:1030) | - | - |
| 1:1 DM avatar with presence badge | src/bundled/channels/ChannelSidebarItem.tsx:186-200 | DONE (crates/app/src/ui/sidebar.rs:243-270) | - | - |
| Agent avatars use squircle shape | src/bundled/channels/ChannelSidebarItem.tsx:197 | MISSING | Shape by `isAgent` | S |
| Group DM shows participant count badge ("N other participants") | src/bundled/channels/ChannelSidebarItem.tsx:202-212 | DIFFERENT: port shows an initials avatar of the joined label | Count badge | S |
| User status shown next to 1:1 DM name (compact) | src/bundled/channels/ChannelSidebarItem.tsx:219-228 | MISSING | Render user status accessory | S |
| DM typing badge ("X is typing...", remote humans, incl. threads) | src/bundled/channels/DmTypingBadge.tsx:8-94 | MISSING | Typing badge on the DM avatar | S |
| Working-agents indicator (turns + channel-level typing, label "<agents> working in <channel>") | src/bundled/channels/working-agents.ts:7-99; src/bundled/channels/ChannelSidebarItem.tsx:238-250 | MISSING | Agent activity feed and three-dot indicator | M |
| Sessions nested under parent channel with collapse, "New session" draft row | src/bundled/channels/ChannelSidebarRow.tsx:96-190; src/features/channel-navigation/ChannelNavigationState.tsx:100-185 | DIFFERENT: session channels render as flat rows among channels | Nest by `parent_channel_id`, collapse state, drafts persisted as `sessions:channel-drafts` | M |
| DM "Remove from Messages" (X button and menu), local viewer-scoped hide with probes that restore on new messages | src/bundled/channels/useHiddenDms.ts:1-237; src/bundled/channels/ChannelSidebarRow.tsx:130-160 | PARTIAL: `LocalHiddenDms` + probe backoff exist (crates/relay/src/sidebar.rs:287-540) but unused; only relay 30622 hides are applied | Wire local hides, X button, focus move, probe loop | M |
| Header menu shares row actions (mute/read/copy link) via navigation state | src/features/channel-navigation/ChannelNavigationState.tsx:20-60 | MISSING | Shared action provider for header menu | S |

### Navigation and links

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Routes: `new-message`, `empty`, `{kind:"new-session", parentId}` | src/features/channel-navigation/routes.ts:3-25 | PARTIAL: new message is a dialog, no new-session route (crates/app/src/ui/navigation.rs) | Add new-session route | S |
| Prepare DM: select the new DM once discovery admits it | src/features/channel-navigation/ChannelNavigationState.tsx:111-130 | DONE (crates/app/src/ui/workspace.rs:721-757) | - | - |
| Public channel search for Cmd-K / `#` completion (180 ms debounce, keep still-matching results while loading, readOnly filter, error text "Public channel search couldn't finish…", retry, partial flag) | src/features/channels/usePublicChannelSearch.ts:26-96 | PARTIAL: search with 180 ms debounce exists (crates/app/src/ui/search.rs:30, 251, 706); no partial flag, no retry, no kept results | Partial/err/retry handling | S |

### Direct messages

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Participant policy: 1–8 unique valid keys, not viewer; "Choose between one and eight other people." | src/features/relay/direct-message-policy.ts:2-12; src/features/relay/direct-messages.ts:66-70 | DONE (crates/relay/src/dm.rs; crates/app/src/ui/new_message.rs:22) | - | - |
| People search paging (15 first browse page, 30 otherwise, prefix search, exact key, viewer excluded, `hasMore`) | src/features/relay/direct-messages.ts:27-65 | DONE (crates/relay/src/dm.rs:56-100; crates/relay/src/session/actions.rs:213-224) | - | - |
| Open DM (41010), strong 39000/39002 confirm, participants check, unhide | src/features/relay/direct-messages.ts:66-102 | DONE (crates/relay/src/session/actions.rs:676-702) | - | - |
| `delivered(id, channelId)`: confirm an earlier first message from journal or relay read, retry failed/unknown, wait on outbox | src/features/relay/direct-messages.ts:103-184 | PARTIAL: generic outbox wait exists (crates/relay/src/session/actions.rs:331); no relay read for a missing journal entry, no "earlier send" recovery | Port the recovery path | S |
| New message as a full page (not a dialog) with composer (mentions, attachments) | src/features/direct-messages/NewMessage.tsx:363-440 | DIFFERENT: modal dialog with plain textarea (crates/app/src/ui/new_message.rs:303-380) | Page with full composer | M |
| Persist recipients and draft per scope; restore after restart ("Could not restore the message.") | src/features/direct-messages/NewMessage.tsx:20-110, 160-205 | MISSING | View-state persistence | S |
| Pending send recovery ("Retry the earlier message to confirm its delivery before editing.", retry button) | src/features/direct-messages/NewMessage.tsx:270-410 | MISSING | Durable pending record and retry UI | M |
| Selected agent no longer available guard | src/features/direct-messages/NewMessage.tsx:226-230 | MISSING | Check agent availability | S |
| Recipient picker: chips, Backspace removes last, Escape, Enter/Tab, "Find a person…" / "Add another person…", "Agent" tag | src/features/direct-messages/RecipientPicker.tsx:290-500 | DONE (crates/app/src/ui/new_message.rs:120-150, 286, 330-336) | - | - |
| Picker "Load more people" / "Retry loading people" paging | src/features/direct-messages/RecipientPicker.tsx:505-540 | MISSING (only page 1, crates/app/src/ui/new_message.rs:98) | Page 2+ and retry | S |
| Picker name-continuation rule (waits while a longer name exists) | src/features/direct-messages/RecipientPicker.tsx:205-230 | MISSING | Match tiering on selection | S |
| Chip removal focus handling | src/features/direct-messages/useChipRemoval.tsx | MISSING | Focus next chip after removal | S |

## Channel members, moderation and profiles

### Channel members (data)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| `canAddMembers`: stream/forum, not archived, member or open public preview | src/features/channel-members/members.ts:19-29 | DONE (crates/relay/src/members.rs:44) | - | - |
| `canShareSession`: add to a private session only | src/features/channel-members/members.ts:33-47 | MISSING | Session share path | S |
| Add member (9000, `role: bot` for agents) through outbox with recovery key `member-add:<channel>:<pubkey>` | src/features/channel-members/members.ts:90-222 | DONE (crates/relay/src/session/actions.rs:574-640; crates/relay/src/members.rs:55-80) | - | - |
| Session participants: agents rejected ("Add agents with the session agent picker instead.") | src/features/channel-members/members.ts:119-128 | MISSING | Comes with session share | S |
| Failed addition older than 15 min must be removed from Outbox first | src/features/channel-members/members.ts:180-186 | MISSING | Expiry check | S |
| Guarded (agent-profile) invitation retry rule | src/features/channel-members/members.ts:161-164 | MISSING | Guard check | S |
| Start added managed agent after confirmed membership (`startAddedAgent`, MembershipChanged error) | src/features/channel-members/members.ts:224-294 | MISSING | Call local agent control after add | M |
| Member search (kind 0 prefix, 30 per page, npub/hex exact, rank by name tier, 256 cap) | src/features/channel-members/search.ts:9-58 | DONE (crates/relay/src/members.rs:218-340) | - | - |
| Administration authority: roles admin/member/guest/owner/bot, canManage/canRemove, authorize, confirmation | src/features/channel-members/administration-protocol.ts:10-207 | DONE (crates/relay/src/members.rs:84-190; crates/relay/src/lifecycle.rs) | - | - |
| Owner may remove own agent with fresh attestation | src/features/channel-members/administration.ts:200-240 | DONE (crates/relay/src/session/actions.rs:648-660) | - | - |
| Uncertain administration result: "This request may have taken effect… it will not be sent again." | src/features/channel-members/administration.ts:38 | PARTIAL: confirm loop exists (crates/relay/src/session/actions.rs:665), no no-resend uncertain state | Track uncertain state | S |
| Single in-flight operation guard ("A member operation is still in progress.") | src/features/channel-members/administration.ts:101 | PARTIAL: dialog busy flag only (crates/app/src/ui/members.rs) | Session-level guard | S |
| Addition receipt tracking across dialog reopen | src/features/channel-members/operations.ts:15-162 | MISSING | Track pending additions and show their state | M |

### Channel members dialog

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Roster grouped Owners/Admins/Members/Agents with presence | src/bundled/channels/ChannelMembersDialog.tsx:632-660 | DONE (crates/app/src/ui/members.rs:30-60) | - | - |
| Role filter "All · N" select | src/bundled/channels/ChannelMembersDialog.tsx:1151-1170 | DONE (crates/app/src/ui/members.rs:591-640) | - | - |
| Search box "Add people and agents" / "Search members"; "Not in this channel" results with Add / "Adding…" | src/bundled/channels/ChannelMembersDialog.tsx:1105-1260 | DONE (crates/app/src/ui/members.rs:160-165, 515-525, 750-775) | - | - |
| Search results "Show more results" paging | src/bundled/channels/ChannelMembersDialog.tsx:1290-1302 | MISSING | Page 2+ | S |
| Disabled-add reasons (DM, archived, unavailable, join first) | src/bundled/channels/ChannelMembersDialog.tsx:1186-1194 | PARTIAL: missing "Adding members is unavailable in this connection." (crates/app/src/ui/members.rs:660-670) | Add the connection case | S |
| Member row menu: View profile, View owner profile, Send message, Make admin/member, Remove | src/bundled/channels/MemberAdministration.tsx:230-300 | DONE (crates/app/src/ui/members.rs:340-400) | - | - |
| Role change and removal confirmations with exact copy | src/bundled/channels/ChannelMembersDialog.tsx:995-1100 | DONE (crates/app/src/ui/members.rs:550-590) | - | - |
| "managed by <owner> (you)" attribution | src/bundled/channels/ChannelMembersDialog.tsx:220-235; useMemberOwners.ts | DONE (crates/app/src/ui/members.rs:418-428) | - | - |
| Archived identities labelled/hidden; "Archived identities could not be checked." | src/bundled/channels/ChannelMembersDialog.tsx:205, 1318-1322 | MISSING | Needs identity archives | S |
| "Role unverified" label | src/bundled/channels/ChannelMembersDialog.tsx:935 | MISSING | Label when role cannot be verified | S |
| Roster load errors ("The member list could not be confirmed." / "could not load."), names-failed note, owners-failed note, "Some agents could not load." | src/bundled/channels/ChannelMembersDialog.tsx:520-565, 1307-1318 | PARTIAL: generic error only | Distinct notes | S |
| Per-person addition error with Retry / "Retrying…" | src/bundled/channels/ChannelMembersDialog.tsx:1324-1338 | MISSING (single error line) | Per-person retry | S |
| "<name> is in the channel." live announcement; "Opening conversation…" status | src/bundled/channels/ChannelMembersDialog.tsx:798, 1340-1344 | MISSING | Status notes | S |
| Refresh member data button | src/bundled/channels/ChannelMembersDialog.tsx:1074 | DONE (crates/app/src/ui/members.rs:789) | - | - |

### Community moderation (Settings → Membership)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Membership settings card visibility (manager / pending / error) | src/bundled/moderation/index.tsx:5-48 | MISSING | Settings section | M |
| NIP-43 roster (13534, relay-authored) read and role derivation | src/features/communities/roster.ts:1-62; src/bundled/moderation/membership.ts:27-150 | MISSING (kind constant only, crates/relay/src/event.rs:26) | Roster reader and roles | M |
| Member list with search, Make admin / Make member / Remove with confirmations and copy | src/bundled/moderation/CommunityAdmin.tsx:250-420 | MISSING | Admin UI and member commands | L |
| Add by npub/hex with role, "already a community member" check | src/bundled/moderation/CommunityAdmin.tsx:425-575 | MISSING | Add-member command | M |
| Invite links: mint, copy, expiry and max-uses options, retry | src/bundled/moderation/CommunityAdmin.tsx:597-720; src/bundled/moderation/api.ts:37-50 | MISSING | Invite mint API and UI | M |
| Stale-list guard "The member list is out of date. Retry before making changes." | src/bundled/moderation/CommunityAdmin.tsx:63 | MISSING | Comes with admin UI | S |

### Profiles

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Kind 0 fold (display_name/name, picture, agent auth tag, fallback name) | src/features/relay/profiles.ts:7-73 | DONE (crates/relay/src/profiles.rs:38-125) | - | - |
| Profile directory: batched 500-author reads, in-flight dedupe, 1024-entry LRU | src/features/relay/profile-directory.ts:25-160 | DONE (crates/relay/src/profiles.rs:13-15, 183-305) | - | - |
| Profile directory 2 MiB byte cap | src/features/relay/profile-directory.ts:25 | MISSING (count cap only) | Byte budget | S |
| Profile selection (per-id stable snapshot) | src/features/relay/profile-selection.ts:1-34 | DONE (snapshot map, crates/relay/src/profiles.rs:183) | - | - |
| Profile panel (side panel, tabs Info / Runtime / Channels / Memories) | src/bundled/profiles/ProfilePanel.tsx:370-620 | DIFFERENT: popover card with avatar, name, key, about, Message (crates/app/src/ui/profile_card.rs:1-90) | Full panel | L |
| Copy npub with status text | src/bundled/profiles/ProfilePanel.tsx:521-540 | MISSING | Copy action | S |
| Presence label (Active / Away / Offline) in profile | src/bundled/profiles/ProfilePanel.tsx:410-415 | MISSING | Show presence | S |
| Profile activity preview ("Latest activity", states) | src/bundled/profiles/ProfileActivity.tsx:44-70; activity-preview.ts | MISSING | Activity feed | M |
| Profile channels list ("No visible channels with verified membership…") | src/bundled/profiles/ProfileChannels.tsx:49-95 | MISSING | Channels list | S |
| Add agent to channel from profile | src/bundled/profiles/ProfileAddChannel.tsx:90-205 | MISSING | Dialog + add + start | M |
| Agent instances, runtime, actions, delete, archive, identity, harness log, memories, public metadata | src/bundled/profiles/ProfileInstances.tsx, ProfileRuntime.tsx, ProfileAgentActions.tsx, ProfileAgentDelete.tsx, ProfileAgentArchive.tsx, ProfileAgentIdentity.tsx, ProfileHarnessLog.tsx, ProfileMemories.tsx, ProfilePublicMetadata.tsx | MISSING from profile (some agent management lives on the Agents page, crates/app/src/ui/agents.rs) | Profile-side agent sections | L |
| Instance profile panel (`parseInstanceTarget`) | src/bundled/profiles/InstanceProfilePanel.tsx | MISSING | Instance panel | M |
| "Message" from profile opens DM ("Could not open the conversation. Try again.") | src/bundled/profiles/ProfilePanel.tsx:330-350 | DONE (crates/app/src/ui/profile_card.rs:45-55; crates/app/src/ui/workspace.rs:737) | - | - |

## Communities, hosted communities and inbox

### Communities

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Destination / relay origin normalization | src/features/communities/destination.ts:1-73 | DONE (crates/relay/src/origin.rs) | - | - |
| Saved communities per viewer, selected community persisted | src/features/communities/service.ts | DONE (crates/app/src/community.rs:49-170) | - | - |
| NIP-11 name ("Buzz Relay" ignored) | src/features/communities/service.ts; CommunityDialog.tsx:233 | DONE (crates/relay/src/community.rs:141-215) | - | - |
| Community icon from NIP-11, cached, fallback to initials | src/features/communities/community-icon.ts:5-68; CommunityRail.tsx:99-128 | MISSING (rail shows initials only, crates/app/src/ui/workspace.rs:405-433) | Fetch icon, save, render | S |
| Invite links: `buzz://join`, share link `https://host/invite/<code>` | src/features/communities/invite-link.ts:1-47 | DONE (crates/relay/src/community.rs:55-130; crates/app/src/deep_links.rs:44) | - | - |
| OS invite intent (one pending request, cleared by request id) | src/features/communities/invite-intent.ts:4-33 | DONE (crates/app/src/deep_links.rs; crates/app/src/ui/add_community.rs) | - | - |
| Join journal (public progress only), resume | src/features/communities/join-journal.ts:1-89 | DONE (crates/relay/src/community.rs:342-420; crates/app/src/ui/add_community.rs:90-145) | - | - |
| Add community dialog: relay URL, invite code, policy (terms, age), profile step | src/features/communities/CommunityDialog.tsx:300-445 | DONE (crates/app/src/ui/add_community.rs:480-700) | - | - |
| Invite error copy (expired / exhausted / invalid / policy changed) | src/features/communities/CommunityDialog.tsx:24-33 | DIFFERENT: port strings differ ("This invite has expired. Ask for a new one." vs "This invite code has expired — ask for a new one."; policy-required/changed cases missing) (crates/relay/src/community.rs:528-545) | Match copy, add policy cases | S |
| Profile default per community, ProfileFields | src/features/communities/profile-default.ts; ProfileFields.tsx:52-70 | DONE (crates/app/src/ui/add_community.rs:640-690) | - | - |
| Avatar URL field on join profile | (not in buzz-app ProfileFields) | DIFFERENT: port adds "Avatar URL (optional)" (crates/app/src/ui/add_community.rs:679) | Remove or keep deliberately | S |
| Leave community (28936), refusals already-absent / revoked, purge device state unless revoked | src/features/communities/leave-protocol.ts:1-59; service.ts:321-375; device-state.ts:64-83 | DONE (crates/relay/src/account.rs:18-280; crates/app/src/community.rs:199-245) | - | - |
| Purge failures reported by store name | src/features/communities/device-state.ts:41-83 | PARTIAL: one store purge, error only logged (crates/app/src/community.rs:237) | Report to user | S |
| Community rail: Personal space button | src/features/communities/CommunityRail.tsx:193-200 | MISSING | Personal space entry | M |
| Rail item context menu: Copy community URL, Invite to community, Community settings, Leave community ("Mark all as read" item: see read state) | src/features/communities/CommunityRailItem.tsx:120-310 | MISSING (leave only in Settings → Communities, crates/app/src/ui/settings/communities.rs:9-130) | Context menu | M |
| Rail tooltip / label "Switch to <name>" | src/features/communities/CommunityRailItem.tsx:159 | MISSING | Tooltip | S |
| `useCommunityRole` (owner/admin from 13534) gating admin menu items | src/features/communities/useCommunityRole.ts:23-69 | MISSING | Role lookup | S |
| Admin protocol (invite request, member command kinds, reasons) | src/features/communities/admin-protocol.ts:11-108 | MISSING | Protocol helpers | M |
| Connection status per community | src/features/communities/connection.ts:1-22 | PARTIAL: session status exists, not shown in rail | Show state | S |

### Hosted communities

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Settings card "Hosted communities" (Builderlab sign-in, account, identity link/unpair, list, quota, create, delete with recovery) | src/bundled/hosted-communities/index.tsx:1-20; HostedCommunities.tsx:120-700; api.ts:1-110; native.ts:8-90 | MISSING | Whole feature | L |

### Inbox

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Inbox page (primary page) with connecting/error/retry states | src/bundled/inbox/index.tsx; InboxPage.tsx:82-115 | MISSING | Page | L |
| Inbox items computation (mentions, threads, DMs) | src/features/relay/inbox.ts:4-30 | PARTIAL: computed in `UnreadState::inbox` (crates/relay/src/unread.rs:664-700, 1798-1935), no consumer | Expose to UI | S |
| Inbox history feed (mentions `#p` kinds 9/40002, 50 per page, cursor until/before_id, edit/delete completeness) | src/features/relay/inbox-feed.ts:1-253 | MISSING | History reader | M |
| Filters (All / Threads / Mentions / DMs / Drafts; From People/Agents; Unread only) | src/bundled/inbox/InboxFilters.tsx:24-120 | MISSING | Filters | M |
| Inbox detail with channel preview and "Open in channel" | src/bundled/inbox/InboxDetail.tsx:160-240; ChannelPreview.tsx | MISSING | Detail view | M |
| Drafts view (list, open in origin, delete with confirm) | src/bundled/inbox/DraftsView.tsx:160-440 | MISSING | Drafts list | M |
| DM labels in inbox | src/bundled/inbox/dm-label.ts | MISSING | Label helper | S |
| Inbox actions expiry/error copy | src/bundled/inbox/InboxPage.tsx:370-385 | MISSING | Comes with page | S |

## Other relay-layer features

Emoji, GIFs, threads, thread windows and typing are close to one to one. Missing entirely: reminders, identity archives, product feedback, Me preferences (beyond a pure model), native agent commands and project Git. Missing wiring: other people's statuses, thread Follow/Unfollow, work-session creation. Paths: buzz-app `src/features/relay/` or `src-tauri/src/relay/`; port `crates/relay/src/`.

### Emoji

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Catalog read `{kinds:[30030],#d:[buzz:custom-emoji],limit:500}` background | emoji-directory.ts:111-150 | DONE (emoji.rs:79-84; session/mod.rs:1514-1531) | - | - |
| 500 sets / 2 MiB budget, over budget = error and empty palette | emoji-directory.ts:29-30, 58-83, 117-121 | DONE (emoji.rs:224-231, 254-286) | - | - |
| "may be incomplete" at ≥500 sets | emoji-directory.ts:134-137 | DONE (emoji.rs:234-250) | - | - |
| Merge: newest set per shortcode, tie smaller URL; `mine` | emoji-directory.ts:89-110 | DONE (emoji.rs:288-313) | - | - |
| Live set replacements | live.ts | DONE (live.rs:485; session/mod.rs:1190) | - | - |
| Lazy `ensure()` load | emoji-directory.ts:151-154, 292-303 | DIFFERENT | Read eagerly at session ready (session/mod.rs:892) | S |
| Reconnect aborts in-flight read and restarts | emoji-directory.ts:285-291 | DIFFERENT | Skips while a read is in flight (session/mod.rs:1516, 1089-1092) | S |
| Re-read after cache clear | emoji-directory.ts:262-280 | PARTIAL | No clear/re-request path | S |
| Send-time `emoji` tags (errors, skip links, first match) | emoji-directory.ts:292-311; emoji.ts:81-113 | DONE (messages.rs:178-255) | - | - |
| Add emoji (fresh own-set read, `created_at = max(now, prev+1)`, 12 s, serialized, "Failed to add emoji." / "Timed out while adding emoji.") | emoji-directory.ts:156-227, 242-259 | PARTIAL | Base on `newer(cached, fresh)`; collapse error text; port also rejects non-http(s) URLs (session/account.rs:195-239; emoji.rs:113-141) | S |
| `suggestShortcode` | emoji.ts:19-30 | DONE (emoji.rs:145-160) | - | - |
| Reaction content rule | emoji.ts:6-10 | DONE (messages.rs:722-746) | - | - |
| Render custom emoji, never inside links | emoji.ts:81-100 | DONE (crates/app/src/ui/markdown.rs:168, 432) | - | - |
| Signer-side `validEmojiSetTemplate` | emoji.ts:32-67 | DIFFERENT | Identity signs any 30030 (crates/identity/src/lib.rs:137-140) | S |

### GIFs

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Capability gate (NIP-11 `buzz-gif`, `klipy`, `/gifs/search`) | gif-capability.ts:10-22; gifs.ts:162-192 | DONE (gifs.rs:19-25; session/gifs.rs:194-208) | - | - |
| HTTPS-only communities | gifs.ts:51-62 | DONE (session/gifs.rs:195, 218) | - | - |
| Per-community discovery cache, forgotten on failure | gifs.ts:156-177, 219-225 | DIFFERENT | NIP-11 read fresh per picker open | S |
| Search body (customer_id, locale, trimmed query; fallback `buzz-desktop`) | gifs.ts:120-130, 206-216 | DONE (gifs.rs:28-35; crates/app/src/ui/gif_picker.rs:60-80) | - | - |
| Result normalization | gifs.ts:64-118 | DONE (gifs.rs:56-106) | - | - |
| Errors ("GIF request failed (status)", "GIF search failed") | gifs.ts:150-153, 217 | DONE (session/gifs.rs:224-236) | - | - |
| Markdown output | gifs.ts:229-237 | DONE (gifs.rs:110-122) | - | - |

### Reminders (kind 30300)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| List read (limit 200) with loading/ready/error and `hydrated` | reminders.ts:241-272 | MISSING | Only `kinds::REMINDERS` (event.rs:34); reminders owner | L |
| Self-decrypt with validation (one `d`, ≤16 KiB, target dialect, field limits, invalid skipped) | src-tauri/src/relay/reminders.rs:30-64, 94-146; reminders.ts:76-120 | MISSING | Decode and validation | M |
| Live 30300 on membership route | reminders.ts:314-320; live.ts:610-615 | PARTIAL | Subscribed (live.rs:496), events dropped | S |
| Head merge (newest, then lowest id) | reminders.ts:172-183 | MISSING | - | S |
| Create / snooze / complete / cancel (expiration 30-90 days) | reminders.ts:122-130, 273-298 | MISSING | Templates and actions | M |
| Ordered edits per coordinate; monotonic created_at | reminders.ts:201-224 | MISSING | - | S |
| Offline/recover gating | reminders.ts:148-153, 300-312 | MISSING | - | S |
| Signer guard (exact tags, time window, expiration, NIP-44 to self) | src-tauri/src/relay/reminders.rs:66-92, 174-240 | MISSING | `Purpose::AppData` allows only 30078 (crates/identity/src/lib.rs:340) | S |
| Reminders page, due badge, alarm marker, `buzz-reminders-checked.v1` | docs/port/ui.md:859, 1344 | MISSING | Depends on owner | L |

### Work sessions and channel creation

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Uncertain creation confirmed by strong id read | work-sessions.ts:60-113 | DONE (session/actions.rs:549-559; lifecycle.rs:671-680) | - | - |
| `delivered()` gives up after 15 s | work-sessions.ts:128-173 | DIFFERENT | Waits indefinitely while Sending (session/actions.rs:331-350) | S |
| Work-session create (private, parent validation, title 1-120) | work-sessions.ts:543-562 | MISSING | Sessions only read (discovery.rs:640-667) | M |
| `invite` (top-level session, library agents) | work-sessions.ts:563-584 | MISSING | - | S |
| `addAgents` (strong roster, parent cascade, archive/library gates, 15-min stale error) | work-sessions.ts:284-317, 388-494 | PARTIAL | Single-channel `add_member` only (session/actions.rs:574-640) | M |
| Refresh gate with expected members/parent, 15 s | work-sessions.ts:185-283 | PARTIAL | Polls channel state (session/actions.rs:316-330) | S |
| `memberChannels` (`#p` 39002 limit 500) | work-sessions.ts:320-353 | MISSING | Needed by agent relay removal | S |
| `listsMember` exact roster read | work-sessions.ts:356-387 | MISSING | - | S |
| `failed` / `discardFailed` for creation | work-sessions.ts:585-604 | PARTIAL | No discard path for create | S |

### Workflow attribution

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| `workflowOwner` rule | workflow-attribution.ts:5-26 | DONE (unread.rs:202-227; fold/mod.rs:112; session/incoming.rs:195) | - | - |
| Notification sender "Workflow · owned by X" | workflow-attribution.ts:29-31 | DONE (crates/app/src/notifications.rs:180-188) | - | - |
| Workflow rows never group | src/features/messages/message-grouping.ts:12-13 | DONE (crates/app/src/ui/message.rs:54-55) | - | - |
| Message row and thread byline (WorkflowByline) | MessageRow.tsx:231-250, 391, 585; ThreadPanel.tsx:338-343 | MISSING | Render in message.rs / thread.rs | S |
| Inbox, popover, search, preview labels | InboxPage.tsx:626-705; SearchResults.tsx:552-562 | PARTIAL | Inbox has `workflow_owner_id` (unread.rs:638), unused; search does not compute it | M |

### Product feedback (kind 42000)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Feedback event (`category`, text, attachments, limits) | product-feedback.ts:1-28 | MISSING | Kind allowed (crates/identity/src/lib.rs:139), never built | M |
| Durable outbox send, availability gate, diagnostics upload | FeedbackDialog.tsx:60-140; session.ts:1723-1743 | MISSING | - | M |
| 42000 excluded from views | session.ts:292, 355, 461, 541 | DONE (threads.rs:111-112, 495) | - | - |

### Identity archives (13535)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Availability (authority = NIP-11 self) | identity-archives.ts:49-63 | MISSING | Only `IDENTITY_ARCHIVE` const (event.rs:27) | S |
| Snapshot read (limit 1, one event, one `-` tag, ≤2 MiB) | identity-archives.ts:68-140 | MISSING | Archives owner | M |
| Rollback fence | identity-archives.ts:57-59, 100-107 | MISSING | - | S |
| `archiveHides` in mentions, agent choices, member add, sessions, search | identity-archives.ts:28-34 | MISSING | Filter pickers | M |
| Consent path (self, owner/admin via 13534, NIP-OA `auth`) | identity-archives.ts:144-179 | MISSING | - | M |
| 9035/9036 request (20 s timeout, re-read confirm) | identity-archive-protocol.ts:8-48; identity-archives.ts:180-233 | MISSING | Kinds allowed (crates/identity/src/lib.rs:138) | M |

### Threads and follows

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| View state machine (initial 10, page 50, ≤10 pages, 2000 events / 4 MiB) | threads.ts:18-33, 300-596 | DONE (threads.rs:95-121, 572-617; threads/window.rs:16-38) | - | - |
| Strict window verification | thread-window.ts:17-158 | DONE (threads/window.rs:197-359; threads.rs:792) | - | - |
| Legacy fallback; non-advancing pagination error | threads.ts:481-551 | DONE (threads.rs:~800-860) | - | - |
| Exact mode | threads.ts:328-395 | DONE (threads.rs:484-492, 598-616) | - | - |
| Live receive with 2-hop aux closure | threads.ts:119-150, 173-240, 264-271 | DONE (threads.rs:521-546) | - | - |
| Purge/revoke; post-revocation reads fenced | threads.ts:277-299 | DONE (threads.rs:548-568; session/threads.rs:136-162) | - | - |
| Repair on re-establish; coalesced refreshes | threads.ts:300-305, 590-593 | DONE (session/threads.rs:202-212; threads.rs:573-578) | - | - |
| 401/403 denies the channel | session.ts | DONE (session/threads.rs:156-161) | - | - |
| Cap of 64 owned thread views | session.ts:1785, 1934 | MISSING | No cap (session/threads.rs:51-109) | S |
| `threadReference` | thread-reference.ts:1-13 | DONE (fold/thread.rs:18-39; unread.rs:140-158) | - | - |
| Follow storage (newest 1000, per partition) | thread-follows.ts:1-63 | DONE (store.rs:804-836; unread.rs:2531-2546) | - | - |
| Forget follows on leave | thread-follows.ts:65-68 | DONE (store.rs:397-409) | - | - |
| Follow/Unfollow thread action | unread.ts follow; docs/port/ui.md:1421 | PARTIAL | `Unread::follow` exists; no Session API or menu item | S |
| Duplicate follows model | n/a | DIFFERENT | threads/follows.rs is dead code with a different key (`thread-follows.v1` vs `thread-follows`); threads.rs:61-64 docs claim wiring | S |

### Typing

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| 8 s lifetime, 2 s quiet after message, 1024 cap | typing.ts:4-6, 67-88 | DONE (typing.rs) | - | - |
| Pulse scope validation, access check | typing.ts:89-124 | DONE (typing.rs) | - | - |
| Completion wins in batch; deadline expiry | typing.ts:31-66, 145-162 | DONE (typing.rs; session/presence.rs:32-47) | - | - |
| Completion evidence from every admitted read | session.ts:551 | DIFFERENT | Live updates only (session/mod.rs:1068-1079) | S |
| Clear on revocation and cache clear | session.ts:369, 2617 | PARTIAL | Only on disconnect (session/presence.rs:66-72) | S |

### User status (30315)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Parse rules (`d=general`, ≤100 UTF-16, expiration) | user-status.ts:17-41; user-status-policy.ts:1-11 | DONE (user_status.rs:91-115) | - | - |
| Save (fresh read, monotonic, 300 s skew, 12 s, one at a time) | user-status.ts:208-284 | DONE (user_status.rs:120-158; session/account.rs:155-189) | - | - |
| Writable gate | user-status.ts:209-215, 329-332 | MISSING | - | S |
| Watched directory for others (refcounted, batches of 100) | user-status.ts:123-207 | MISSING | Only `own_status` | M |
| Live 30315 updates | user-status.ts:96-122 | PARTIAL | Subscribed (live.rs:482), dropped | S |
| Expiry timer republish | user-status.ts:68-95 | MISSING | - | S |
| Background retries 5/15/60 s | user-status.ts:149-161 | MISSING | - | S |
| `current(userId)` for others; reload on reconnect/clear | user-status.ts:310-350 | PARTIAL | Viewer only (session/account.rs:136-151) | S |
| Status on profiles, member lists, rows | UI consumers | MISSING | Only the account panel | M |

### Me preferences (Me groups kit)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Me kit record read/decode/save with revision checks | me-preferences.ts:72-123 | MISSING | Kit owner | L |
| `placement.has/admit/set` with capacity message | me-preferences.ts:42-69, 198-240 | MISSING | - | M |
| Section create/assign/delete through sidebar store | me-preferences.ts:124-186 | MISSING | - | M |
| Projection: personal groups replace legacy sections; Me channels leave main sidebar | sidebar-personal-groups.ts | PARTIAL | Pure model only (sidebar.rs:807-840) | S |

### Native agent operations and project Git

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| `relay_agent_resolve` owner proof | src-tauri/src/relay/agent.rs:45-58 | MISSING | - | S |
| `relay_agent_log_proof` | agent.rs:68-112 | MISSING | - | S |
| Observer telemetry 24200 decrypt | agent.rs:124-232; native.ts:612-628 | MISSING | - | M |
| Archive 44200 decode and archive settings | agent.rs:203-232; native.ts:596-640 | MISSING | - | M |
| Agent memories 30174 read (limits, HMAC d-tag, newest per slug, `MemoryDenied`) | agent.rs:317-497; native.ts:666-690 | MISSING | - | L |
| `relay_agent_library` (legacy managed-agents.json listing) | agent.rs:499-636 | DIFFERENT | crates/host/src/import.rs:51-80 reads it for import only | M |

## Channel page, settings, lifecycle, create, canvas, templates and usage

The relay side of lifecycle and details editing matches well (templates, permissions, owner-agent evidence, confirmation polling). The UI is much thinner: no tab pane, settings panel, duration control, privacy confirmation, uncertain-save recovery, Canvas, templates or usage view. Paths: buzz-app `src/bundled/channels/` (BC), `src/features/relay/` (FR), `src/features/channel-templates/` (FT), `src/bundled/channel-templates/` (BT), `src/bundled/channel-usage/` (BU); port `crates/relay/src/` (R) and `crates/app/src/ui/` (A).

### Channel page layout and states

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Disconnected/error splash ("Your channels, one conversation.") | BC/ChannelsPage.tsx:128-148 | DIFFERENT | No community goes to onboarding (A/workspace.rs:145-160); no splash for disconnected/failed community | S |
| Default selection: saved `selected-channel`, else first non-session channel | BC/ChannelsPage.tsx:207-209, 348-356, 1495 | PARTIAL | Only remembered channel restored (A/workspace.rs:179-202); else empty text (A/workspace.rs:459-472) | S |
| Routed unjoined channel: exact `resolve()` preview, "Checking conversation access…" | BC/ChannelsPage.tsx:258-296, 1486-1497 | PARTIAL | Waits for roster then logs and drops (A/workspace.rs:661-672, 693-718); search already resolves (A/search.rs:692) | M |
| "Loading messages…" placeholder | BC/ChannelBody.tsx:96-105 | DONE | - | - |
| Timeline error with "Retry messages" | BC/ChannelBody.tsx:81-95 | PARTIAL | Error text only (A/channel.rs:716) | S |
| Header: channel tab, "Channel members", "Channel actions" | BC/ChannelsPage.tsx:1301-1405 | DONE | - | - |
| Read-only preview notice and "Join channel" | BC/ChannelJoinNotice.tsx:9-62; BC/ChannelsPage.tsx:1499-1525 | DIFFERENT | Also requires `visibility==Public` (A/channel_menu.rs:631-635); replaces composer instead of sitting above it (A/channel.rs:579-584) | S |
| Join outcome ("Join may have taken effect… Try again to check.") and composer focus | BC/ChannelJoinNotice.tsx:31-48; BC/ChannelsPage.tsx:363-371 | PARTIAL | Generic UNCONFIRMED text (R/session/actions.rs:35); no composer focus (A/channel_menu.rs:605-626) | S |
| After Leave/Delete/Hide select next row; Archive keeps page | BC/ChannelsPage.tsx:340-347 | DONE (A/channel_menu.rs:127-133; A/workspace/dialogs.rs:126-142) | - | - |
| Settings "Diagnostics" (UnreadOptions, "Partial roster · N channels", Refresh channels / groups and stars / messages, Outbox, timings) | BC/ChannelsPage.tsx:1163-1200 | MISSING | - | M |

### Conversation tabs, picker and panels

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Toggle tab pane" button, tab close with fallback and focus return | BC/ChannelsPage.tsx:1406-1426; BC/useConversationTabs.tsx:297-321 | MISSING | Tab pane workspace | M |
| Tab state per session and channel kept across visits | BC/useChannelTabState.ts:1-164 | PARTIAL | One thread panel per workspace, cleared on channel switch (A/workspace.rs:233, 257-308; A/thread.rs:556-587) | M |
| "New tab" picker (Channels/DMs/Tools, fuzzy, usage-ranked, keyboard) | BC/ChannelTabPicker.tsx:49-229 | MISSING | - | L |
| Secondary conversation tabs and thread tabs ("This conversation is no longer available.") | BC/ConversationTab.tsx:1-181; BC/useConversationTabs.tsx:322-379, 430-486 | MISSING | - | L |
| Plugin channel launchers, drawer and side panel | BC/useChannelPanels.tsx:1-141 | MISSING | Plugin panel host | L |
| Channel-menu plugin panels (e.g. "View channel usage") as tabs | BC/ChannelsPage.tsx:1357-1404; BC/useConversationTabs.tsx:153-170, 487-504 | MISSING | - | M |
| Link routing into panels; legacy session links into a tab | BC/ChannelsPage.tsx:955-1050 | PARTIAL | `buzz://` navigation only (A/workspace.rs:789-817) | M |

### Channel settings panel and details editor

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Channel settings" tab (identity, Escape closes, "Editing is unavailable on this connection.") | BC/ChannelSettingsPanel.tsx:31-224; BC/ChannelsPage.tsx:1561-1585 | DIFFERENT | Modal "Channel details" dialog (A/channel_menu.rs:379-581) | M |
| Description and Visibility rows (hidden for DM/session), click to edit | BC/ChannelSettingsPanel.tsx:103-181 | PARTIAL | Fields shown directly, disabled for non-editors; Description shown for DMs (A/channel_menu.rs:485-546) | S |
| Members row with count; Channel ID row with copy ("Channel ID copied") | BC/ChannelSettingsPanel.tsx:182-192, 308-356 | MISSING | Port has "Copy link" instead (A/channel_menu.rs:157-171) | S |
| Setup tools (Save as template, "Personal group" select, lifecycle buttons) | BC/ChannelsPage.tsx:1092-1158 | MISSING | - | M |
| Personal groups settings card ("Manage personal groups", "Reload groups") | BC/ChannelSetupSettings.tsx:1-105 | MISSING | - | M |
| Fresh load, reload keeping explicit choices, "Reload details" on error | BC/ChannelDetailsEditor.tsx:122-168, 292-301 | PARTIAL | Loads once; error replaces body, no reload (A/channel_menu.rs:405-423, 484) | S |
| Save gating (dirty and valid) and inline field errors | BC/ChannelDetailsEditor.tsx:173-197, 492-521; FR/channel-details-protocol.ts:45-76 | PARTIAL | Save always enabled; errors only after submit (A/channel_menu.rs:436-449, 565-577; R/lifecycle.rs:613-633) | S |
| "Discard changes?" (Keep editing / Discard changes) | BC/ChannelDetailsEditor.tsx:218-234, 384-395 | MISSING | - | S |
| Privacy change confirmation ("Make channel private?/public?") | BC/ChannelDetailsEditor.tsx:396-441, 474-479 | MISSING | - | S |
| Uncertain save survives reopen ("Check save status", "Check the previous change before saving again.") | FR/channel-details.ts:31-32, 142-153, 162-163, 206-211; BC/ChannelDetailsEditor.tsx:258-303, 450-458 | MISSING | Port returns UNCONFIRMED and allows immediate resave (R/session/actions.rs:511-540) | M |
| Normalization and copy | BC/ChannelDetailsEditor.tsx:173-176 | DIFFERENT | Port trims description (A/channel_menu.rs:442); shows "This channel's details can't be changed now." for any TTL channel even when the reason is role (A/channel_menu.rs:547-555) | S |

### Shared form fields

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| ChannelTextField (counter at ≥90 %, input bounded by code points, SR count) | BC/ChannelTextField.tsx:22-34, 81-98 | DIFFERENT | Always shows N/limit and allows overflow (A/new_channel.rs:121-136; A/channel_menu.rs:486-525) | S |
| ChannelDurationField (Ongoing/Temporary, 1d/1w/2w steps) | BC/ChannelDurationField.tsx:8-131 | MISSING | - | M |
| ChannelPrivacyConfirmation with "Don't show me this again" per scope | BC/ChannelPrivacyConfirmation.tsx:6-49 | MISSING | - | S |

### Details protocol

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| `channelVisibility` and `canonicalDetailsName` | FR/channel-details-protocol.ts:26-42 | DONE (R/discovery.rs:180; R/lifecycle.rs:607-611) | - | - |
| Strong 39000/39001/39002 read, canEdit, TTL parse | FR/channel-details-protocol.ts:98-134, 181-186; FR/channel-details.ts:82-101 | DONE (R/lifecycle.rs:134-152, 194-207, 383-407; buzz-app also rejects non-members) | - | - |
| 9002 template (visibility/ttl only when changed) | FR/channel-details-protocol.ts:136-178 | DONE (R/lifecycle.rs:709-744) | - | - |
| Session channels: name-only 9002 `[h,name]` | FR/channel-details-protocol.ts:78-96, 142-154, 189-212 | MISSING | Port always sends `about`; `validate_about` rejects the session marker (R/lifecycle.rs:622-633, 709-744) | S |
| Authorize before and after signing; error copy | FR/channel-details.ts:166-200 | PARTIAL | Checks once (R/session/actions.rs:511-533); texts differ | S |
| Confirm by re-read and adopt metadata | FR/channel-details.ts:102-120 | DONE (R/session/actions.rs:534-540; R/lifecycle.rs:747-761) | - | - |
| Access gate ("Channel access changed. Reopen channel settings.") and 20 s timeout | FR/channel-details.ts:51-81 | PARTIAL | 20 s there (R/session/actions.rs:102-115); no access revocation check | S |

### Lifecycle UI

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Header lifecycle menu (Hide / Leave / Archive / Unarchive / Delete check unavailable / Delete) | BC/ChannelLifecycleMenu.tsx:22-168 | PARTIAL | Items there (A/channel_menu.rs:102-116, 179-255); missing "Retry channel permissions" / "Retry Delete check" and "unavailable on this connection"; port shows "Checking…" | S |
| Settings-panel lifecycle buttons with retry, "No channel actions available." | BC/ChannelLifecycleActions.tsx:15-162 | MISSING | - | S |
| Confirmation dialog copy, Delete destructive, no close while busy | BC/ChannelLifecycleDialog.tsx:10-36, 82-118 | DONE (A/channel_menu.rs:259-313, 331-345) | - | - |
| After unconfirmed result, confirm stays disabled | BC/ChannelLifecycleDialog.tsx:56-76, 104 | MISSING | Confirm stays enabled, action can be resent (A/channel_menu.rs:300-310) | S |

### Lifecycle protocol

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Templates archive/unarchive 9002, delete 9008, join 9021, leave 9022, hide 41012 | FR/channel-lifecycle-protocol.ts:23-73 | DONE (R/lifecycle.rs:490-513) | - | - |
| Permissions incl. last-owner leaveReason | FR/channel-lifecycle-protocol.ts:110-180 | DONE (R/lifecycle.rs:212-290, 330-351; port also cross-checks 39001 vs 39002) | - | - |
| Owner-agent evidence (kind 0, batches of 4, 5 s) | FR/channel-lifecycle.ts:133-218 | DONE (R/session/actions.rs:405-447) | - | - |
| Re-authorize after signing, single in-flight guard, error copy | FR/channel-lifecycle.ts:286-363 | PARTIAL | Authorizes once, no global busy guard, generic "You can't do that in this channel." (R/session/actions.rs:459-483) | S |
| Join preconditions | FR/channel-lifecycle.ts:302-320 | DIFFERENT | Specific texts instead of "Only active public channels can be joined." (R/lifecycle.rs:572-600) | S |
| Confirmation polling 0/250/750/1500 ms | FR/channel-lifecycle.ts:369-424 | DONE (R/lifecycle.rs:23-28, 516-569; R/session/actions.rs:484-509) | - | - |
| Unconfirmed error text with reason | FR/channel-lifecycle.ts:20-27, 426-428 | DIFFERENT | Different string, no reason, also on timeout (R/session/actions.rs:35, 102-115) | S |
| DM visibility snapshot (status, newer wins, invalid list = error) | FR/channel-lifecycle.ts:219-282 | PARTIAL | No status or error; invalid lists ignored (R/session/actions.rs:970-1010; R/lifecycle.rs:763-802) | S |

### Create channel

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Name/description, canonical name, Private switch, Create disabled when invalid | BC/CreateChannelDialog.tsx:133-184, 379-412, 446-465 | PARTIAL | Works (A/new_channel.rs:63-113, 168-185); no inline errors or "Buzz session (" pre-check | S |
| Temporary duration (ttl) | BC/CreateChannelDialog.tsx:135-138, 225, 550-560 | MISSING | NewChannel has no ttl (R/lifecycle.rs:636-668) | M |
| Privacy confirmation and "Discard changes?" | BC/CreateChannelDialog.tsx:186-199, 341-399 | MISSING | (A/new_channel.rs:140-144, 168-178) | S |
| Pending creation recovery ("Retry channel", restore after restart) | BC/CreateChannelDialog.tsx:157-175, 411, 434-439; FT/setup.ts:130-190 | MISSING | Waits inside dialog only (R/session/actions.rs:542-572) | M |
| Group destination ("Create a channel in <group>") | BC/CreateChannelDialog.tsx:142-145, 207-216, 311-327 | MISSING | - | M |
| Template/teams editor and setup summary | BC/CreateChannelDialog.tsx:253-282, 466-549, 562 | MISSING | - | L |
| 9007 via outbox, receipt, roster confirm, open channel; gated on relay support | docs/port/relay.md §5m.1 | DONE (R/session/actions.rs:542-572; A/new_channel.rs:79-113; A/workspace.rs:486-489) | - | - |

### Canvas

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "View canvas" and Canvas dialog (Markdown, saved draft `canvas-draft-v1`, "Save Canvas") | BC/ChannelCanvasDialog.tsx:13-243; BC/ChannelHeaderMenu.tsx:99-108 | MISSING | - | L |
| Conflict handling ("Canvas changed since this draft began…", "Discard and reload") | BC/ChannelCanvasDialog.tsx:58-124, 155-166, 248-272; FT/canvas-conflict.ts:1-22 | MISSING | - | M |
| Save protocol (40100 `expected-revision`, 24 KiB, 1 s spacing) | FT/capability.ts:426-528; FT/canvas-signing-contract.json | MISSING | Only kind const (R/event.rs:47) | M |
| History (paged strong read of 25, restore confirm, >24 KiB not restorable) | BC/CanvasHistory.tsx:26-315; FT/capability.ts:443-486 | MISSING | - | M |

### Channel completion (#)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Joined-channel completion (tiered match, limit 20, escaped link, "Private channel") | BC/ChannelCompletion.tsx:11-37, 48-55, 124-145 | PARTIAL | Mostly done (A/composer.rs:805-845, 1379-1391); missing newline-name filter, membership/cached check, id tiebreak | S |
| Open unjoined channels ("Not joined", 5 rows reserved) | BC/ChannelCompletion.tsx:38-41, 67-117 | MISSING | - | M |
| Status/retry lines, duplicate-name id detail, canSelect recheck | BC/ChannelCompletion.tsx:87-92, 119-176 | MISSING | - | S |

### Live status and relay timings

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|

### Channel templates

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Plugin registration and gating (Templates card, providers) | BT/index.tsx:1-28; FT/provider.ts:1-88 | MISSING | - | M |
| Channel kit store (30078 NIP-44, 16 KiB, decode checks, model validation) | FT/capability.ts:50-425; FT/model.ts:13-291; FT/host.ts | MISSING | - | L |
| Template library and dialog | BT/TemplateLibrary.tsx; BT/TemplateSettings.tsx; BT/ChannelTemplatesDialog.tsx | MISSING | - | L |
| Template selection and fields (agent search, teams, Starting Canvas, group default) | BT/TemplateEditor.tsx:1-270; BT/TemplateFields.tsx:1-232 | MISSING | - | M |
| Portable team payload (16 KiB chunks, 8 MiB) | FT/team-payload.ts:1-180 | MISSING | - | M |
| Setup orchestration (`buzz-channel-setup.v2`) | FT/setup.ts:46-409 | MISSING | - | L |
| Save channel as template; catalog hook | BT/TemplateSettings.tsx:80+; BT/useTemplateCatalog.ts:1-38 | MISSING | - | M |

### Channel usage

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "View channel usage" panel and usage archive read (2,000-record cap) | BU/index.tsx:7-46; BU/use-usage-archive.ts:1-160 | MISSING | No agent-activity archive | M |
| Usage view (agent/session pickers, totals, tokens, cache, USD, per-turn list) | BU/ChannelUsage.tsx:1-301 | MISSING | - | L |

## DONE count

**DONE: 223** of 562 rows.

Other statuses: PARTIAL 93, DIFFERENT 57, MISSING 189.

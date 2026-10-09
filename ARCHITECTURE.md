# Architecture

Buzz GPUI is a native client for Buzz communities, written with
[GPUI](https://github.com/zed-industries/zed). It covers the chat and
agent-management parts of [buzz-app](https://github.com/block/buzz-app) with
the same UI, and adds agents that run on other machines.

## Processes

```
buzz-gpui (laptop)                              buzzd (dev server)
├─ UI (GPUI + gpui-kit, crates/ui, crates/app)  ├─ agent host (crates/host)
├─ relay client (crates/relay)                  └─ RPC server (crates/host-rpc)
├─ human key (OS keyring, never leaves)
├─ embedded agent host (crates/host)
└─ host registry ── host-rpc ─┬─ in-memory ──► embedded host
                              └─ TCP / Unix ─► buzzd
```

- Chat goes from the GUI directly to the relay. Agents also talk to the relay
  directly, as their own identities, through `buzz-acp`. A host never carries
  chat traffic.
- A host owns everything that is local to a machine: the agent controller and
  process supervision, agent keys, harness detection, and logs.
- The GUI always runs one embedded host for agents on this machine. It talks to
  the embedded host over the same protocol as a remote one, through an
  in-memory transport, so both paths run the same code.
- Remote hosts are `buzzd` processes. The transport is a trait. TCP and Unix
  sockets ship first; Tailscale (or anything else) is set up in front of the
  TCP port.
- When a host needs the human's signature (the owner attestation on agent
  create or import), it asks the GUI over RPC. The human key never leaves the
  GUI process.

## Host protocol (crates/host-rpc)

- Frames: a big-endian `u32` length, then a MessagePack body with named fields.
  Unknown fields are ignored, so either side can add fields without breaking
  the other.
- Handshake: `Hello { protocol, token, client }` →
  `Welcome { protocol, host }` or `Rejected { reason }`. A token is required on
  every listener except the in-memory one.
- Requests and responses pair by `id`. The host pushes events, such as agent
  state changes, without polling.
- Reconnect is state-based, not replay-based. After a reconnect the client asks
  for a fresh snapshot. Events carry a sequence number, so the client can drop
  stale ones. Host actions are idempotent, so a client may retry a request
  after a reconnect.
- Heartbeat: the client pings every 5 s. Three missed pongs close the
  connection, and the client reconnects with backoff.

## Relay client (crates/relay)

This is a port of buzz-app's TypeScript relay layer (`src/features/relay`).
It uses the same wire behavior (NIP-42 WebSocket, NIP-98 HTTP `/query`), the
same budgets, and the same outbox semantics. It is backed by SQLite instead of
IndexedDB and localStorage. UI views read from in-memory projections that are
updated incrementally; no projection is rebuilt from scratch on each event.

## Reused code

- From buzz-app: `buzz-agent-controller`, `buzz-credential-store` and
  `buzz-pairing`, as git dependencies pinned to a revision.
- buzz-app's TypeScript and its Tauri host are the reference for behavior. We
  do not depend on them.
- Zed's GPL crates are not used and not copied.

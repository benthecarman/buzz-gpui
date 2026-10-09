#!/usr/bin/env bash
# Throwaway, isolated Buzz relay for developing buzz-gpui.
#
#   scripts/test-relay.sh up       build (if needed), start deps, migrate, start relay
#   scripts/test-relay.sh seed     identities, members, channels, messages (idempotent)
#   scripts/test-relay.sh verify   WS (NIP-42) + HTTP (NIP-98) smoke checks
#   scripts/test-relay.sh status   show deps/relay state
#   scripts/test-relay.sh logs     tail relay log (`logs deps` for docker logs)
#   scripts/test-relay.sh down     stop relay and deps (data kept)
#   scripts/test-relay.sh wipe     down + delete DB/minio data and seed.json
#
# Everything lives in .test-relay/ (gitignored). See scripts/TEST-RELAY.md.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE="${ROOT}/.test-relay"
SRC="${STATE}/buzz"
BUZZ_REPO="https://github.com/block/buzz"
BUZZ_REV="5142cf6b2a0f44686eeda1ed07905ee3560fd78e"
PROJECT="buzz-gpui-test"
COMPOSE_FILE="${ROOT}/scripts/test-relay.compose.yml"

RELAY_PORT=53000
HEALTH_PORT=58080
METRICS_PORT=59102
PG_PORT=55432
REDIS_PORT=56379
MINIO_PORT=59000

# The community is keyed by this exact authority. Clients must connect with
# Host: 127.0.0.1:53000 ("localhost:53000" is a different, unmapped host).
COMMUNITY_HOST="127.0.0.1:${RELAY_PORT}"
WS_URL="ws://${COMMUNITY_HOST}"
HTTP_URL="http://${COMMUNITY_HOST}"
DATABASE_URL="postgres://buzz:buzz_dev@127.0.0.1:${PG_PORT}/buzz"
REDIS_URL="redis://127.0.0.1:${REDIS_PORT}"

IDENTITIES="${STATE}/identities.json"
SEED_FILE="${STATE}/seed.json"
RELAY_ENV="${STATE}/relay.env"
RELAY_LOG="${STATE}/relay.log"
RELAY_PID="${STATE}/relay.pid"
BIN="${SRC}/target/debug"

export TEST_RELAY_STATE="${STATE}"
export TEST_RELAY_UID="$(id -u)"
export TEST_RELAY_GID="$(id -g)"

log() { printf '\033[0;34m[test-relay]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[0;31m[test-relay]\033[0m %s\n' "$*" >&2; exit 1; }

compose() { docker compose -p "${PROJECT}" -f "${COMPOSE_FILE}" "$@"; }

# ── tools ────────────────────────────────────────────────────────────────────

NAK=""
nak() {
  if [[ -z "${NAK}" ]]; then
    if type -P nak >/dev/null 2>&1; then
      NAK="$(type -P nak)"
    else
      NAK="$(nix build nixpkgs#nak --no-link --print-out-paths 2>/dev/null)/bin/nak"
    fi
    [[ -x "${NAK}" ]] || die "could not get nak (nix build nixpkgs#nak failed)"
  fi
  "${NAK}" "$@"
}

ident() { jq -r --arg n "$1" --arg f "$2" '.identities[$n][$f]' "${IDENTITIES}"; }

# ── source + build ───────────────────────────────────────────────────────────

ensure_src() {
  if [[ ! -d "${SRC}/.git" ]]; then
    log "Fetching ${BUZZ_REPO} @ ${BUZZ_REV:0:12}..."
    mkdir -p "${SRC}"
    git -C "${SRC}" init -q
    git -C "${SRC}" remote add origin "${BUZZ_REPO}"
    git -C "${SRC}" fetch -q --depth 1 origin "${BUZZ_REV}"
    git -C "${SRC}" checkout -q FETCH_HEAD
  fi
  # Upstream hardcodes 0.0.0.0 for the health and metrics listeners; keep the
  # test relay loopback-only. (Idempotent local patch to the throwaway clone.)
  sed -i 's/bind(("0\.0\.0\.0", config\.health_port))/bind(("127.0.0.1", config.health_port))/' \
    "${SRC}/crates/buzz-relay/src/main.rs"
  sed -i 's/with_http_listener((\[0, 0, 0, 0\], port))/with_http_listener(([127, 0, 0, 1], port))/' \
    "${SRC}/crates/buzz-relay/src/metrics.rs"
  grep -q 'bind(("127.0.0.1", config.health_port))' "${SRC}/crates/buzz-relay/src/main.rs" \
    || die "health-listener loopback patch did not apply"
  grep -q 'with_http_listener((\[127, 0, 0, 1\], port))' "${SRC}/crates/buzz-relay/src/metrics.rs" \
    || die "metrics-listener loopback patch did not apply"
}

build() {
  ensure_src
  log "Building buzz-relay, buzz-admin, buzz (cli), buzz-test-cli (dev profile)..."
  # rust-toolchain.toml pins 1.95; the system toolchain (newer) builds it fine.
  # cmake is needed by audiopus_sys; the rest are belt and braces.
  (cd "${SRC}" && nix shell nixpkgs#cmake nixpkgs#pkg-config nixpkgs#protobuf --command \
    cargo build -q -p buzz-relay -p buzz-admin -p buzz-cli -p buzz-test-client) \
    >>"${STATE}/build.log" 2>&1 || die "cargo build failed; see ${STATE}/build.log"
}

# ── identities + relay env ───────────────────────────────────────────────────

ensure_identities() {
  [[ -f "${IDENTITIES}" ]] && return 0
  log "Generating throwaway identities -> ${IDENTITIES}"
  local entries=() name role sk pk
  for spec in alice:owner bob:member carol:member; do
    name="${spec%%:*}"; role="${spec##*:}"
    sk="$(nak key generate)"
    pk="$(nak key public "${sk}")"
    entries+=("$(jq -n --arg name "${name}" --arg role "${role}" --arg sk "${sk}" --arg pk "${pk}" \
      --arg nsec "$(nak encode nsec "${sk}")" --arg npub "$(nak encode npub "${pk}")" \
      '{($name): {role: $role, display_name: ($name | (.[:1] | ascii_upcase) + .[1:]),
                  sk_hex: $sk, nsec: $nsec, pk_hex: $pk, npub: $npub}}')")
  done
  printf '%s\n' "${entries[@]}" | jq -s \
    --arg ws "${WS_URL}" --arg http "${HTTP_URL}" --arg host "${COMMUNITY_HOST}" \
    '{note: "TEST-ONLY throwaway keys for the local buzz-gpui test relay",
      relay_ws_url: $ws, relay_http_url: $http, community_host: $host,
      identities: add}' > "${IDENTITIES}"
  chmod 600 "${IDENTITIES}"
}

ensure_relay_env() {
  [[ -f "${RELAY_ENV}" ]] && return 0
  log "Writing ${RELAY_ENV} (stable relay signing key)"
  umask 077
  cat > "${RELAY_ENV}" <<EOF
BUZZ_RELAY_PRIVATE_KEY=$(nak key generate)
EOF
}

relay_env() {
  # shellcheck disable=SC1090
  source "${RELAY_ENV}"
  cat <<EOF
DATABASE_URL=${DATABASE_URL}
REDIS_URL=${REDIS_URL}
RELAY_URL=${WS_URL}
BUZZ_BIND_ADDR=127.0.0.1:${RELAY_PORT}
BUZZ_HEALTH_PORT=${HEALTH_PORT}
BUZZ_METRICS_PORT=${METRICS_PORT}
BUZZ_RELAY_PRIVATE_KEY=${BUZZ_RELAY_PRIVATE_KEY}
BUZZ_REQUIRE_RELAY_MEMBERSHIP=true
RELAY_OWNER_PUBKEY=$(ident alice pk_hex)
BUZZ_REQUIRE_AUTH_TOKEN=false
BUZZ_RECONCILE_CHANNELS=true
BUZZ_PUSH_ENABLED=false
BUZZ_GIT_CONFORMANCE_PROBE=false
BUZZ_GIT_REPO_PATH=${STATE}/git-repos
BUZZ_S3_ENDPOINT=http://127.0.0.1:${MINIO_PORT}
BUZZ_S3_ACCESS_KEY=buzz_dev
BUZZ_S3_SECRET_KEY=buzz_dev_secret
BUZZ_S3_BUCKET=buzz-media
BUZZ_S3_REGION=us-east-1
BUZZ_S3_ADDRESSING_STYLE=path
RUST_LOG=${RUST_LOG:-info,buzz_relay=debug,buzz_auth=debug}
EOF
}

with_relay_env() {
  local envs=()
  mapfile -t envs < <(relay_env)
  env "${envs[@]}" "$@"
}

# ── deps + relay lifecycle ───────────────────────────────────────────────────

relay_running() {
  [[ -f "${RELAY_PID}" ]] && kill -0 "$(cat "${RELAY_PID}")" 2>/dev/null
}

relay_ready() {
  [[ "$(curl -s -o /dev/null -w '%{http_code}' "${HTTP_URL}/_readiness" || true)" == "200" ]]
}

port_in_use() { ss -ltnH "sport = :$1" 2>/dev/null | grep -q .; }

deps_up() {
  mkdir -p "${STATE}/data/postgres" "${STATE}/data/minio" "${STATE}/git-repos"
  log "Starting deps (compose project ${PROJECT})..."
  compose up -d --quiet-pull >&2
  log "Waiting for postgres + redis..."
  local pg redis
  for _ in $(seq 1 60); do
    pg="$(docker inspect -f '{{.State.Health.Status}}' buzz-gpui-test-postgres 2>/dev/null || true)"
    redis="$(docker inspect -f '{{.State.Health.Status}}' buzz-gpui-test-redis 2>/dev/null || true)"
    [[ "${pg}" == healthy && "${redis}" == healthy ]] && break
    sleep 1
  done
  [[ "${pg}" == healthy && "${redis}" == healthy ]] || die "deps not healthy (pg=${pg} redis=${redis})"
  for _ in $(seq 1 60); do
    [[ "$(docker inspect -f '{{.State.Status}}' buzz-gpui-test-minio-init 2>/dev/null)" == exited ]] && break
    sleep 1
  done
}

migrate() {
  log "Running migrations (buzz-admin migrate)..."
  with_relay_env "${BIN}/buzz-admin" migrate >&2
}

relay_start() {
  if relay_running; then
    log "Relay already running (pid $(cat "${RELAY_PID}"))"
    return 0
  fi
  for p in "${RELAY_PORT}" "${HEALTH_PORT}" "${METRICS_PORT}"; do
    port_in_use "${p}" && die "port ${p} already in use by something else; refusing to start"
  done
  log "Starting buzz-relay on ${WS_URL} (log: ${RELAY_LOG})"
  (cd "${STATE}" && with_relay_env nohup "${BIN}/buzz-relay" >>"${RELAY_LOG}" 2>&1 & echo $! > "${RELAY_PID}")
  for _ in $(seq 1 90); do
    relay_running || { tail -40 "${RELAY_LOG}" >&2; die "relay exited during startup"; }
    relay_ready && { log "Relay ready: ${WS_URL}"; return 0; }
    sleep 1
  done
  die "relay not ready after 90s; see ${RELAY_LOG}"
}

relay_stop() {
  if relay_running; then
    local pid; pid="$(cat "${RELAY_PID}")"
    log "Stopping relay (pid ${pid})"
    kill "${pid}" 2>/dev/null || true
    for _ in $(seq 1 20); do kill -0 "${pid}" 2>/dev/null || break; sleep 0.5; done
    kill -9 "${pid}" 2>/dev/null || true
  fi
  rm -f "${RELAY_PID}"
}

cmd_up() {
  mkdir -p "${STATE}"
  command -v jq >/dev/null || die "jq required"
  build
  ensure_identities
  ensure_relay_env
  deps_up
  migrate
  relay_start
  cat <<EOF
Relay:      ${WS_URL}  (HTTP ${HTTP_URL})
Identities: ${IDENTITIES}
Next:       scripts/test-relay.sh seed
EOF
}

cmd_down() {
  relay_stop
  log "Stopping deps (compose project ${PROJECT})..."
  compose down >&2
}

cmd_wipe() {
  cmd_down
  log "Deleting ${STATE}/data, git-repos, seed.json (identities + relay key kept)"
  rm -rf "${STATE}/data" "${STATE}/git-repos" "${SEED_FILE}"
}

cmd_status() {
  echo "== deps (${PROJECT})"
  compose ps -a --format 'table {{.Name}}\t{{.State}}\t{{.Status}}\t{{.Ports}}' || true
  echo "== relay"
  if relay_running; then
    echo "pid $(cat "${RELAY_PID}") running; readiness: $(relay_ready && echo ready || echo NOT ready)"
    echo "NIP-11:"
    curl -s -H 'Accept: application/nostr+json' "${HTTP_URL}/" | jq -c '{name, pubkey, supported_nips, software, version}' 2>/dev/null || true
  else
    echo "not running"
  fi
  [[ -f "${SEED_FILE}" ]] && { echo "== seed (${SEED_FILE})"; jq -c '.channels' "${SEED_FILE}"; }
  return 0
}

cmd_logs() {
  if [[ "${1:-}" == deps ]]; then compose logs -f --tail 100; else tail -n 200 -F "${RELAY_LOG}"; fi
}

# ── seed + verify ────────────────────────────────────────────────────────────

# Run the repo's own CLI (`buzz`, HTTP + NIP-98) as a named identity.
buzz_as() {
  local who="$1"; shift
  BUZZ_RELAY_URL="${HTTP_URL}" BUZZ_PRIVATE_KEY="$(ident "${who}" sk_hex)" "${BIN}/buzz" "$@"
}

require_ready() {
  relay_running && relay_ready || die "relay not running; run: scripts/test-relay.sh up"
  [[ -f "${IDENTITIES}" ]] || die "missing ${IDENTITIES}"
}

cmd_seed() { die "TODO"; }
cmd_verify() { die "TODO"; }

case "${1:-}" in
  up) cmd_up ;;
  down) cmd_down ;;
  wipe) cmd_wipe ;;
  status) cmd_status ;;
  logs) shift; cmd_logs "$@" ;;
  seed) shift; cmd_seed "$@" ;;
  verify) cmd_verify ;;
  build) build ;;
  *) sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac

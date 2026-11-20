#!/usr/bin/env bash
# Runs on a Boat sandbox, as `user`. Idempotent; also used to upgrade a machine.
#   T3BOAT_SECRET=<watchdog-secret> [T3BOAT_TAILSCALE_KEY_FILE=<path>] \
#     provision.sh <t3-version> <sleep-after-secs> <snapshot-name> <mint-upstream-token:0|1> <name> \
#     <fresh-from-snapshot:0|1>
# Expects watchdog.mjs next to it. Prints one JSON line: {"environmentId", "upstreamToken"?}.
# T3BOAT_TAILSCALE_KEY_FILE: a file holding a Tailscale auth key; set only when creating a
# machine with Tailscale on. The key never appears in an argument list.
set -euo pipefail

version="$1" sleep_after="$2" snapshot="$3" mint_upstream="$4" name="$5" fresh="$6"
secret="$T3BOAT_SECRET"

# A new machine started from a saved snapshot, maybe another t3boat machine's: drop that
# machine's identity before it can act as it (its watchdog would snapshot over the source's
# snapshot), its T3 environment id, auth secrets and chats. The t3 binary, the user's repos and
# tools stay; they are what the snapshot is for.
if [ "$fresh" = 1 ]; then
  for unit in t3boat-watchdog.service t3code.service; do
    systemctl --user stop "$unit" >/dev/null 2>&1 || true
  done
  rm -rf "$HOME/.t3/userdata" "$HOME/.t3boat"
fi

here="$(cd "$(dirname "$0")" && pwd)"
t3="$HOME/.local/bin/t3"
boat_dir="$HOME/.t3boat"
units="$HOME/.config/systemd/user"
mkdir -p "$boat_dir" "$units" "$HOME/.t3/userdata"
chmod 700 "$boat_dir"

# T3 server pinned to the desktop app's version, as a systemd service that survives resumes.
# It is restarted only when something it runs on changed: a restart drops every T3 Code
# connection, and most upgrades only touch the watchdog.
restart_t3=0
# A snapshot's own t3 binary is not trusted: a share code can come from anyone.
if [ "$fresh" = 1 ] || [ "$("$t3" --version 2>/dev/null || true)" != "t3 v$version" ]; then
  if ! curl -fsSL https://t3.codes/install.sh | T3CODE_VERSION="$version" sh >"$boat_dir/install.log" 2>&1; then
    echo "T3 install failed" >&2
    tail -n 40 "$boat_dir/install.log" >&2
    exit 1
  fi
  restart_t3=1
fi
settings="$HOME/.t3/userdata/settings.json"
before="$(cat "$settings" 2>/dev/null || true)"
node -e '
  const fs = require("fs"), f = process.argv[1];
  const s = fs.existsSync(f) ? JSON.parse(fs.readFileSync(f, "utf8")) : {};
  s.linkedEnvironmentsEnabled = true;
  fs.writeFileSync(f, JSON.stringify(s, null, 2));
' "$settings"
[ "$before" = "$(cat "$settings")" ] || restart_t3=1
# The machine's name, as other devices show it (T3 Connect, linked environments). T3 reads it
# from here as it starts; without it, they show the sandbox's hostname.
machine_info="PRETTY_HOSTNAME=$name"
if [ "$(cat /etc/machine-info 2>/dev/null || true)" != "$machine_info" ]; then
  echo "$machine_info" | sudo tee /etc/machine-info >/dev/null
  restart_t3=1
fi
"$t3" service install --allow-downgrade >/dev/null
systemctl --user daemon-reload
systemctl --user is-active --quiet t3code.service || restart_t3=1
[ "$restart_t3" = 0 ] || systemctl --user restart t3code.service

ready=0
for _ in $(seq 1 120); do
  if curl -sf -o /dev/null http://127.0.0.1:3773/.well-known/t3/environment; then ready=1; break; fi
  sleep 0.5
done
if [ "$ready" != 1 ]; then
  echo "T3 server did not start" >&2
  tail -n 40 "$HOME/.t3/userdata/logs/boot-service.log" >&2 || true
  exit 1
fi

# Watchdog: its own read-only T3 token, kept across upgrades.
config="$boat_dir/config.json"
watchdog_token="$(node -e 'try{process.stdout.write(require(process.argv[1]).t3Token||"")}catch{}' "$config")"
if [ -z "$watchdog_token" ]; then
  watchdog_token="$("$t3" auth session issue --token-only --ttl 3650d --label t3boat-watchdog --scope orchestration:read)"
fi
SECRET="$secret" TOKEN="$watchdog_token" SLEEP="$sleep_after" SNAPSHOT="$snapshot" node -e '
  const e = process.env;
  require("fs").writeFileSync(process.argv[1], JSON.stringify({
    secret: e.SECRET, t3Token: e.TOKEN, sleepAfterSecs: Number(e.SLEEP), snapshot: e.SNAPSHOT
  }, null, 2), { mode: 0o600 });
' "$config"
install -m 600 "$here/watchdog.mjs" "$boat_dir/watchdog.mjs"
cat >"$units/t3boat-watchdog.service" <<'UNIT'
[Unit]
Description=t3boat watchdog: stops this sandbox when idle
After=t3code.service

[Service]
ExecStart=/bin/bash -lc 'exec node --no-warnings %h/.t3boat/watchdog.mjs'
Restart=always
RestartSec=5
StandardOutput=append:%h/.t3boat/watchdog.log
StandardError=append:%h/.t3boat/watchdog.log

[Install]
WantedBy=default.target
UNIT
systemctl --user daemon-reload
systemctl --user enable t3boat-watchdog.service >/dev/null 2>&1
systemctl --user restart t3boat-watchdog.service

# Tailscale. With a key: installed and joined to the user's tailnet with Tailscale SSH, unless
# already logged in. Without one (every upgrade): left exactly as it is, but for one case. The
# owner marker names the machine that joined (its snapshot name, unique per machine) and lives
# with tailscaled's own state, so it travels exactly when that state does. A disk copied from
# another machine carries that machine's node key: its state is wiped here, on the machine only.
# Never `tailscale logout`: that would expire the key the original machine still uses and take
# it off the tailnet. Joined anew only with a key. No --accept-routes: a tailnet route over
# Boat's own networks (10.77.0.0/16, 192.168.122.0/24) would cut the machine off.
ts_key="${T3BOAT_TAILSCALE_KEY_FILE:-}"
ts_dir=/var/lib/tailscale
ts_owner="$ts_dir/t3boat-owner"
ts_state() {
  sudo tailscale status --json 2>/dev/null \
    | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{process.stdout.write(JSON.parse(s).BackendState||"")}catch{}})' \
    || true
}
if sudo test -f "$ts_owner" && [ "$(sudo cat "$ts_owner")" != "$snapshot" ]; then
  sudo systemctl stop tailscaled >/dev/null 2>&1 || true
  sudo rm -rf "$ts_dir"
  # Started again (as a new, logged-out node) only below, with a key.
fi
if [ -n "$ts_key" ]; then
  if ! command -v tailscale >/dev/null; then
    if ! curl -fsSL https://tailscale.com/install.sh | sh >"$boat_dir/tailscale-install.log" 2>&1; then
      echo "Tailscale install failed" >&2
      tail -n 40 "$boat_dir/tailscale-install.log" >&2
      exit 1
    fi
  fi
  if ! sudo systemctl enable --now tailscaled >/dev/null 2>&1; then
    echo "Tailscale: tailscaled did not start" >&2
    exit 1
  fi
  case "$(ts_state)" in
    Running | Starting) ;;
    *)
      if ! sudo tailscale up --auth-key="file:$ts_key" --hostname="t3boat-$name" --ssh --timeout=2m >&2; then
        echo "Tailscale: joining the tailnet failed; check the auth key in t3boat's Settings (reusable, pre-authorized, not expired)" >&2
        exit 1
      fi
      ;;
  esac
  echo "$snapshot" | sudo tee "$ts_owner" >/dev/null
fi

upstream_token=""
if [ "$mint_upstream" = 1 ]; then
  upstream_token="$("$t3" auth session issue --token-only --ttl 3650d --label t3boat)"
fi
environment_id="$(curl -sf http://127.0.0.1:3773/.well-known/t3/environment | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.parse(s).environmentId))')"
ENV_ID="$environment_id" TOK="$upstream_token" node -e '
  const o = { environmentId: process.env.ENV_ID };
  if (process.env.TOK) o.upstreamToken = process.env.TOK;
  console.log(JSON.stringify(o));
'

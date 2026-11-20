#!/usr/bin/env bash
# Runs on a machine through `boat exec`, as `user`, once it is on the user's tailnet
# (`tailnet::ensure_tailnet_serve`). Idempotent: run after a join and on every wake.
#   tailscale-serve.sh <may-restart-t3:0|1> <snapshot-name>
# First starts tailscaled, which never starts at boot (a disk copied to a new machine would come
# up as this node), when the disk is this machine's own: its owner marker is <snapshot-name>.
# Turns on T3's own Tailscale Serve support, so T3 publishes itself as
# https://<MagicDNS name>/ and advertises that endpoint: `T3CODE_TAILSCALE_SERVE=true` in T3's
# service.env, which T3's service launcher reads at every start (`t3 service install` rewrites
# the unit, never that file). T3 runs `tailscale serve` as this user, so the user becomes the
# tailnet node's operator. Nothing changes when the tailnet has HTTPS certificates off.
# T3 is restarted only when Serve is not on yet, and only if allowed (a restart stops agent turns).
# Prints one JSON line last: {"state", "dnsName"}, state being
#   on         T3 has Tailscale Serve on and it forwards to T3
#   starting   T3 has it on but Serve does not forward yet (first certificate, say)
#   busy       a T3 restart is needed and was not allowed
#   httpsOff   HTTPS certificates are off on the tailnet
#   offTailnet Tailscale is not running here
set -euo pipefail

may_restart="$1"
snapshot="$2"
user="$(id -un)"
port=3773

say() {
  node -e 'console.log(JSON.stringify({ state: process.argv[1], dnsName: process.argv[2] || "" }))' "$1" "${2:-}"
}

ts_owner=/var/lib/tailscale/t3boat-owner
if command -v tailscale >/dev/null && sudo test -f "$ts_owner" && [ "$(sudo cat "$ts_owner")" = "$snapshot" ]; then
  sudo systemctl disable tailscaled >/dev/null 2>&1 || true
  if ! sudo systemctl is-active --quiet tailscaled; then
    sudo systemctl start tailscaled
    for _ in $(seq 1 30); do
      sudo tailscale status --peers=false >/dev/null 2>&1 && break
      sleep 1
    done
  fi
fi

# BackendState, MagicDNS name (no trailing dot) and whether the tailnet issues HTTPS certificates
# (the node's CapMap has "https"), as `T3`'s own check reads them. Tab-separated.
status="$(sudo tailscale status --json --peers=false 2>/dev/null \
  | node -e '
    let s = ""; process.stdin.on("data", d => s += d).on("end", () => {
      let j = {}; try { j = JSON.parse(s) } catch {}
      const self = j.Self || {}, caps = self.CapMap;
      const https = caps && typeof caps === "object" && !Array.isArray(caps) && Object.hasOwn(caps, "https");
      process.stdout.write([j.BackendState || "", String(self.DNSName || "").replace(/\.$/, ""), https ? 1 : 0].join("\t"));
    })' || true)"
IFS=$'\t' read -r backend dns https <<<"$status" || true
if [ "${backend:-}" != Running ] || [ -z "${dns:-}" ]; then
  say offTailnet
  exit 0
fi
case "$dns" in *[!a-z0-9.-]*) echo "Tailscale reported an odd MagicDNS name" >&2; exit 1 ;; esac
if [ "${https:-0}" != 1 ]; then
  say httpsOff "$dns"
  exit 0
fi

# T3 calls `tailscale serve` without sudo: only the operator may. A `tailscale up --reset` (a
# fresh join) clears it, so it is checked every time.
operator="$(sudo tailscale debug prefs 2>/dev/null \
  | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{process.stdout.write(JSON.parse(s).OperatorUser||"")}catch{}})' || true)"
operator_set=0
if [ "$operator" != "$user" ]; then
  sudo tailscale set --operator="$user"
  operator_set=1
fi

# T3's home, as its unit names it; service.env lives there.
unit="$HOME/.config/systemd/user/t3code.service"
home="$(sed -n 's/^Environment=T3CODE_HOME=//p' "$unit" 2>/dev/null | head -n 1 || true)"
home="${home:-$HOME/.t3}"
env_file="$home/service.env"
want="T3CODE_TAILSCALE_SERVE=true"
env_set=0
if ! grep -qx "$want" "$env_file" 2>/dev/null; then
  mkdir -p "$home"
  tmp="$(mktemp "$home/service.env.XXXXXX")"
  { grep -vE '^[[:space:]]*(export[[:space:]]+)?T3CODE_TAILSCALE_SERVE[[:space:]]*=' "$env_file" 2>/dev/null || true
    echo "$want"; } >"$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$env_file"
  env_set=1
fi

# Serve's root route on 443 forwards to T3, as T3 sets it up once it starts.
serving() {
  sudo tailscale serve status --json 2>/dev/null \
    | node -e '
      let s = ""; process.stdin.on("data", d => s += d).on("end", () => {
        let j = null; try { j = JSON.parse(s) } catch {}
        const [dns, port] = process.argv.slice(1);
        const site = ((j || {}).Web || {})[dns + ":443"] || {};
        const proxy = ((site.Handlers || {})["/"] || {}).Proxy || "";
        const target = proxy.replace(/^https?:\/\//, "").replace(/\/$/, "");
        process.exit(target === "127.0.0.1:" + port || target === "localhost:" + port ? 0 : 1);
      })' "$dns" "$port"
}
wait_serving() {
  for _ in $(seq 1 "$1"); do
    if serving; then return 0; fi
    sleep 1
  done
  return 1
}

# Already on: T3 started with the setting and Serve forwards to it (it sets Serve up a few
# seconds after it starts, so a machine that just woke gets a moment).
if [ "$env_set" = 0 ] && [ "$operator_set" = 0 ] && wait_serving 20; then
  say on "$dns"
  exit 0
fi
if [ "$may_restart" != 1 ]; then
  say busy "$dns"
  exit 0
fi
systemctl --user restart t3code.service
ready=0
for _ in $(seq 1 120); do
  if curl -sf -o /dev/null "http://127.0.0.1:$port/.well-known/t3/environment"; then ready=1; break; fi
  sleep 0.5
done
if [ "$ready" != 1 ]; then
  echo "T3 did not come back after turning on Tailscale Serve" >&2
  tail -n 40 "$home/userdata/logs/boot-service.log" >&2 || true
  exit 1
fi
if wait_serving 45; then
  say on "$dns"
else
  say starting "$dns"
fi

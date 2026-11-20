#!/usr/bin/env bash
# Runs on a machine through `boat exec`, as `user`: the first step of joining the user's tailnet
# by signing in with the browser (`tailnet::join_tailnet`). No auth key, ever.
#   tailscale-connect.sh <name> <snapshot-name>
# Installs Tailscale if missing and applies provision.sh's copied-disk rule. Already on a
# tailnet: says so. Otherwise starts an interactive `tailscale up` in the background (it outlives
# this exec) and waits up to 30s for its sign-in URL. Prints one JSON line last:
# {"BackendState": "...", "AuthURL": "..."}. Tailnet routes stay off, as in provision.sh.
set -euo pipefail

name="$1" snapshot="$2"
boat_dir="$HOME/.t3boat"
ts_dir=/var/lib/tailscale
ts_owner="$ts_dir/t3boat-owner"
mkdir -p "$boat_dir"

ts_status() {
  sudo tailscale status --json --peers=false 2>/dev/null \
    | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{let j={};try{j=JSON.parse(s)}catch{}process.stdout.write(JSON.stringify({BackendState:j.BackendState||"",AuthURL:j.AuthURL||""}))})' \
    || true
}

# A disk copied from another machine carries that machine's node key: wiped here, never logged out.
if sudo test -f "$ts_owner" && [ "$(sudo cat "$ts_owner")" != "$snapshot" ]; then
  sudo systemctl stop tailscaled >/dev/null 2>&1 || true
  sudo rm -rf "$ts_dir"
fi
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

st=""
for _ in $(seq 1 20); do
  st="$(ts_status)"
  case "$st" in *'"BackendState":""'*) sleep 0.5 ;; *) break ;; esac
done
case "$st" in
  *'"BackendState":"Running"'* | *'"BackendState":"Starting"'*)
    echo "$st"
    exit 0
    ;;
esac

# An unfinished sign-in from before gives way to this one. `[r]` keeps pkill off its own sudo.
sudo pkill -f 'tailscale up --[r]eset --hostname=t3boat-' >/dev/null 2>&1 || true
log="$boat_dir/tailscale-up.log"
nohup sudo tailscale up --reset --hostname="t3boat-$name" --ssh --timeout=15m >"$log" 2>&1 </dev/null &

for _ in $(seq 1 30); do
  sleep 1
  st="$(ts_status)"
  case "$st" in
    *'"BackendState":"Running"'* | *'"BackendState":"NeedsMachineAuth"'*) break ;;
    *'"AuthURL":""'*) ;;
    *'"AuthURL":"'*) break ;;
  esac
done
case "$st" in
  *'"BackendState":"Running"'* | *'"BackendState":"NeedsMachineAuth"'*) ;;
  *'"AuthURL":""'* | "")
    echo "Tailscale gave no sign-in link" >&2
    tail -n 20 "$log" >&2 || true
    exit 1
    ;;
esac
echo "$st"

<a href="https://t3boat.paukraft.com">
  <img src="apps/web/public/og.png" alt="t3boat: Run your T3 Code agents in the cloud, on Boat">
</a>

<p align="center">
  <a href="https://t3boat.paukraft.com">Website</a> ·
  <a href="#install">Install</a> ·
  <a href="apps/desktop/docs/architecture.md">How it works</a>
</p>

t3boat creates cloud machines on your [Boat](https://boat.dev) account and adds them to [T3 Code](https://t3.codes). They sleep when idle, so you only pay for the minutes your agents run.

Chat in T3 Code as usual. A message to a sleeping machine wakes it; running work finishes with your laptop closed.

## What you get

- Create, resize and delete machines from the menu bar
- Add projects the way T3 Code's "Add to another machine" does, `.env` files included
- Sign Claude Code or Codex in on a machine with their own login
- T3 Connect, to reach a machine from your phone or browser
- Cost per machine, today and this month
- An MCP server, so agents on your computer can manage machines
- Optionally, machines on your Tailscale tailnet with Tailscale SSH

## What you need

- [T3 Code](https://t3.codes) desktop app
- A [Boat](https://boat.dev) account (t3boat walks you through the CLI sign-in)
- macOS, Windows or Linux

## Install

No prebuilt download yet. Build from source with [Rust](https://rustup.rs) and [Bun](https://bun.sh). Windows also needs the [Microsoft C++ Build Tools](https://v2.tauri.app/start/prerequisites/#windows), Linux [Tauri's system packages](https://v2.tauri.app/start/prerequisites/#linux) (Debian/Ubuntu: `sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev build-essential file`).

```sh
git clone https://github.com/paukraft/t3boat && cd t3boat
bun install
bun run build:desktop
```

The app lands in `apps/desktop/src-tauri/target/release/bundle/`:

- **macOS**: copy `macos/t3boat.app` to `/Applications` and open it. It lives in the menu bar.
- **Windows**: run `nsis/t3boat_*-setup.exe`. It lives in the system tray, maybe behind the overflow arrow.
- **Linux**: install `deb/t3boat_*.deb` or run the AppImage in `appimage/`. It lives in the system tray; without one (GNOME without AppIndicator) it opens as a window.

Or let your agent do it:

```
Please build and install t3boat (https://github.com/paukraft/t3boat) on my computer: clone it, follow the Install section of its README, and open the app when it's done.
```

## Usage

1. Click the boat in the menu bar and create a machine.
2. Accept the new backend in T3 Code.
3. Work in T3 Code as usual. Push or open a PR from the machine.

To let Claude Code or Codex on your computer manage machines, click "Add" next to them in t3boat's settings.

## Tailscale

t3boat can put machines on your [Tailscale](https://tailscale.com) tailnet, with [Tailscale SSH](https://tailscale.com/kb/1193/tailscale-ssh) on, so you can `ssh user@t3boat-<name>` from your own devices. T3 Code keeps connecting the usual way; Tailscale is extra.

In t3boat's Settings → Tailscale, pick **Browser**. Each new machine then opens a Tailscale sign-in page once it is up; click Connect there and it joins as `t3boat-<name>`. Creating a machine never waits for you: the sign-in comes after. A machine from before, or one whose sign-in you skipped, has a Connect button in its details (and "Connect to tailnet" in its ⋯ menu); it wakes for it and stays awake while you sign in.

A machine joined this way belongs to you, like a laptop you signed in on: its key expires after 180 days unless you disable key expiry for it in the admin console. Tailscale's default access rules already let you SSH to your own devices (`autogroup:self`, with a browser check now and then); with custom rules, allow `user` on them.

A sleeping machine shows as offline and comes back when it wakes. Deleting an awake machine logs it out of the tailnet; delete an asleep one and remove its offline node in the admin console.

### Advanced: auth key

For machines that join without a sign-in and never expire, pick **Auth key** instead:

1. In the Tailscale admin console, define a tag for the machines and an SSH rule (Access controls):

   ```jsonc
   {
     "tagOwners": {
       "tag:t3boat": ["autogroup:admin"]
     },
     "ssh": [
       {
         "action": "accept",
         "src": ["autogroup:member"],
         "dst": ["tag:t3boat"],
         "users": ["user"]
       }
     ]
   }
   ```

2. Under Settings → Keys, generate an auth key that is **reusable**, **pre-approved** and **tagged** `tag:t3boat` (tagged nodes don't expire).
3. Paste it in t3boat's Settings → Tailscale and click Save. It is kept in your system keychain (macOS Keychain), nowhere else.

Machines created from then on join with it as `t3boat-<name>` while they are created. Connect on an existing machine still signs in with the browser.

## Development

```sh
bun run dev           # website (apps/web)
bun run dev:desktop   # menu bar app (apps/desktop, Tauri)
```

Architecture: [apps/desktop/docs/architecture.md](apps/desktop/docs/architecture.md).

## License

[MIT](LICENSE). Made by [Pau Kraft](https://x.com/paukraft). Not affiliated with or endorsed by T3 Code or Boat.

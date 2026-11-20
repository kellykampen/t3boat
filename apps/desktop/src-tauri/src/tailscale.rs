//! Joining machines to the user's tailnet, with Tailscale SSH. Two ways, picked in Settings
//! (`store::TailscaleMode`):
//! - browser sign-in (the main one): `connect_script` starts an interactive `tailscale up` on the
//!   machine, the app opens its sign-in URL here and waits for the node to be Running. No key.
//! - an auth key (advanced): it lives in the OS credential store (the macOS Keychain) and nowhere
//!   else on this computer: not the state file, not logs, not error messages. A machine gets it as
//!   a file inside the provisioning upload (`provision::run`), never on a command line.

use crate::machine::provision::shell_word;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

const SERVICE: &str = "t3boat";
const ACCOUNT: &str = "tailscale-auth-key";

/// An auth key. Its `Debug` hides it, so a stray `{:?}` can't log it.
#[derive(Clone)]
pub struct AuthKey(String);

impl AuthKey {
    /// What the user pasted, trimmed. The message never echoes it.
    pub fn parse(key: &str) -> Result<Self> {
        let key = key.trim();
        if !key.starts_with("tskey-") || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            bail!("that doesn't look like a Tailscale auth key (they start with tskey-)");
        }
        Ok(Self(key.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AuthKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthKey(…)")
    }
}

fn entry() -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(describe)
}

/// keyring's errors can carry the stored bytes (`BadEncoding`); none of those reach a message.
fn describe(e: keyring::Error) -> anyhow::Error {
    match e {
        keyring::Error::BadEncoding(_) => anyhow!("the Tailscale key in the keychain is unreadable; save it again"),
        e => anyhow!("keychain: {e}"),
    }
}

/// The saved key, if any. Blocking: the keychain may ask the user first.
pub fn load() -> Result<Option<AuthKey>> {
    match entry()?.get_password() {
        Ok(key) => Ok(Some(AuthKey(key))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(describe(e)),
    }
}

pub fn save(key: &AuthKey) -> Result<()> {
    entry()?.set_password(key.expose()).map_err(describe)
}

/// Removing a key that isn't there is fine.
pub fn remove() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(describe(e)),
    }
}

/// Run on the machine before its sandbox is deleted: takes the node off the tailnet. Only a
/// node t3boat joined (it left the owner marker, next to tailscaled's state) is touched.
pub const LOGOUT_SCRIPT: &str =
    "if sudo test -f /var/lib/tailscale/t3boat-owner && command -v tailscale >/dev/null; then sudo tailscale logout; fi";

/// Where the owner marker lives: next to tailscaled's own state, so it travels exactly when that does.
const OWNER_MARKER: &str = "/var/lib/tailscale/t3boat-owner";

const CONNECT_SH: &str = include_str!("machine/tailscale-connect.sh");

/// The first step of a browser sign-in, run on the machine: installs Tailscale if needed and
/// starts `tailscale up --hostname=t3boat-<name> --ssh` unless already on a tailnet. Its output
/// ends in a `Status` line, with the sign-in URL when there is one. `snapshot`: the machine's
/// snapshot name, its owner marker (a copied disk with another one gets a fresh node).
pub fn connect_script(name: &str, snapshot: &str) -> Result<String> {
    Ok(format!("set -- {} {}\n{CONNECT_SH}", shell_word(name)?, shell_word(snapshot)?))
}

/// Polled while the user signs in.
pub const STATUS_COMMAND: &str = "sudo tailscale status --json --peers=false";

/// Stops a `tailscale up` still waiting for a sign-in (a cancel, a timeout). `[r]`: pkill
/// would otherwise match the `sudo` running it.
pub const STOP_SIGN_IN_SCRIPT: &str = "sudo pkill -f 'tailscale up --[r]eset --hostname=t3boat-' || true";

/// Run once the machine is on the tailnet: marks the node as t3boat's (see `LOGOUT_SCRIPT`).
pub fn owner_script(snapshot: &str) -> Result<String> {
    Ok(format!("echo {} | sudo tee {OWNER_MARKER} >/dev/null", shell_word(snapshot)?))
}

/// What `tailscale status --json` says about this node (ipnstate.Status), or the line
/// `connect_script` prints with the same two fields.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Status {
    /// "NoState", "NeedsLogin", "NeedsMachineAuth", "Stopped", "Starting" or "Running".
    #[serde(rename = "BackendState", default)]
    pub backend_state: String,
    /// The sign-in page, while a `tailscale up` waits for one.
    #[serde(rename = "AuthURL", default)]
    pub auth_url: String,
}

impl Status {
    /// The whole output as one JSON document, or else its last line that is one.
    pub fn parse(out: &str) -> Result<Self> {
        if let Ok(s) = serde_json::from_str(out.trim()) {
            return Ok(s);
        }
        let line = out
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| l.starts_with('{'))
            .ok_or_else(|| anyhow!("Tailscale gave no status"))?;
        serde_json::from_str(line).context("reading Tailscale's status")
    }

    pub fn running(&self) -> bool {
        self.backend_state == "Running"
    }

    /// An admin has to approve the device in the admin console (device approval is on).
    pub fn needs_approval(&self) -> bool {
        self.backend_state == "NeedsMachineAuth"
    }

    /// The sign-in page, if it is one to open. The machine reports it, and a disk started from
    /// someone else's snapshot is untrusted, so only Tailscale's own sign-in page (https, no
    /// credentials, default port, under /a/) is ever handed to the browser.
    pub fn sign_in_url(&self) -> Option<&str> {
        let raw = self.auth_url.trim();
        let url = url::Url::parse(raw).ok()?;
        let ok = url.scheme() == "https"
            && url.host_str() == Some(SIGN_IN_HOST)
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path().starts_with("/a/")
            // Exactly what was checked, nothing a browser could read differently (backslashes,
            // whitespace, odd encodings are normalised by the parser, so they fail this).
            && url.as_str() == raw;
        ok.then_some(raw)
    }
}

/// Where Tailscale's coordination server sends people to approve a device. t3boat only supports
/// Tailscale's own control server, not a self-hosted one.
const SIGN_IN_HOST: &str = "login.tailscale.com";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_trims_and_never_echoes() {
        assert_eq!(AuthKey::parse("  tskey-auth-k1-abc\n").unwrap().expose(), "tskey-auth-k1-abc");
        let e = AuthKey::parse("tskey-auth-k1 $(rm -rf ~)").unwrap_err().to_string();
        assert!(!e.contains("rm -rf"));
        assert!(AuthKey::parse("hunter2").is_err());
        assert_eq!(format!("{:?}", AuthKey::parse("tskey-auth-k1-abc").unwrap()), "AuthKey(…)");
    }

    #[test]
    fn status_while_waiting_for_sign_in() {
        // Trimmed `tailscale status --json --peers=false` of a node whose `tailscale up` waits.
        let out = r#"{
  "Version": "1.90.6-t1234",
  "TUN": true,
  "BackendState": "NeedsLogin",
  "HaveNodeKey": true,
  "AuthURL": "https://login.tailscale.com/a/1a2b3c4d5e6f",
  "TailscaleIPs": null,
  "Self": { "ID": "", "HostName": "t3boat-dev", "Online": false },
  "Health": ["You are logged out."],
  "MagicDNSSuffix": "",
  "CurrentTailnet": null
}"#;
        let s = Status::parse(out).unwrap();
        assert_eq!(s.backend_state, "NeedsLogin");
        assert!(!s.running());
        assert_eq!(s.sign_in_url(), Some("https://login.tailscale.com/a/1a2b3c4d5e6f"));
    }

    #[test]
    fn status_running_and_script_line() {
        let running = Status::parse(r#"{"BackendState":"Running","AuthURL":"","Self":{"HostName":"t3boat-dev"}}"#).unwrap();
        assert!(running.running());
        assert_eq!(running.sign_in_url(), None);
        // connect_script: whatever it logged, then its status line.
        let out = "installing…\n{\"BackendState\":\"NeedsLogin\",\"AuthURL\":\"https://login.tailscale.com/a/xyz\"}\n";
        assert_eq!(Status::parse(out).unwrap().sign_in_url(), Some("https://login.tailscale.com/a/xyz"));
        assert!(Status::parse(r#"{"BackendState":"NeedsMachineAuth"}"#).unwrap().needs_approval());
        assert!(Status::parse("tailscale: command not found").is_err());
        // Only https reaches the browser.
        let odd = Status { backend_state: "NeedsLogin".into(), auth_url: "file:///etc/passwd".into() };
        assert_eq!(odd.sign_in_url(), None);
        for bad in [
            "https://evil.example/a/x",
            "https://login.tailscale.com.evil.example/a/x",
            "https://user@login.tailscale.com/a/x",
            "https://login.tailscale.com:8443/a/x",
            "https://login.tailscale.com/admin/settings/keys",
            "http://login.tailscale.com/a/x",
            "https://login.tailscale.com\\@evil.example/a/x",
            "https://login.tailscale.com/a/x y",
            "https://LOGIN.tailscale.com/a/x",
            "https://login.tailscale.com/a/../admin",
        ] {
            let st = Status { auth_url: bad.into(), ..Default::default() };
            assert_eq!(st.sign_in_url(), None, "{bad}");
        }
    }

    #[test]
    fn connect_script_has_no_key_and_checks_its_words() {
        let s = connect_script("dev", "t3boat-dev-bx-abc").unwrap();
        assert!(s.starts_with("set -- dev t3boat-dev-bx-abc\n"));
        assert!(s.contains("--hostname=\"t3boat-$name\" --ssh"));
        assert!(!s.contains("auth-key") && !s.contains("tskey") && !s.contains("TAILSCALE_KEY"));
        assert!(!s.contains("--accept-routes"));
        assert!(connect_script("dev; rm -rf ~", "t3boat-dev-bx").is_err());
        assert!(connect_script("dev", "$(id)").is_err());
        assert!(owner_script("a b").is_err());
        assert_eq!(owner_script("t3boat-dev-bx").unwrap(), "echo t3boat-dev-bx | sudo tee /var/lib/tailscale/t3boat-owner >/dev/null");
    }
}

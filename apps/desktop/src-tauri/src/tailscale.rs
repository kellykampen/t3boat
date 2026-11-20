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

/// Where HTTPS certificates are turned on for a tailnet; Tailscale Serve needs them.
pub const HTTPS_SETTINGS_URL: &str = "https://login.tailscale.com/admin/dns";

const SERVE_SH: &str = include_str!("machine/tailscale-serve.sh");

/// Run on a machine on the tailnet: turns on T3's own Tailscale Serve (see the script). Its
/// output ends in a `ServeReport` line. `may_restart`: T3 may be restarted to take the setting,
/// which stops agent turns.
pub fn serve_script(may_restart: bool) -> String {
    format!("set -- {}\n{SERVE_SH}", u8::from(may_restart))
}

/// What `serve_script` found.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ServeState {
    /// T3 has Tailscale Serve on, and Serve forwards to it.
    On,
    /// T3 has it on, Serve does not forward yet (a first certificate, say).
    Starting,
    /// T3 must restart to take it, and may not now (agents work).
    Busy,
    /// HTTPS certificates are off on the tailnet: nothing changed.
    HttpsOff,
    /// Tailscale does not run on the machine.
    OffTailnet,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServeReport {
    pub state: ServeState,
    /// The machine's MagicDNS name, e.g. `t3boat-dev.tail1234.ts.net`.
    #[serde(default)]
    pub dns_name: String,
}

impl ServeReport {
    /// The last line of the script's output that is one.
    pub fn parse(out: &str) -> Result<Self> {
        let line = out
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| l.starts_with('{'))
            .ok_or_else(|| anyhow!("the Tailscale Serve setup said nothing"))?;
        serde_json::from_str(line).context("reading the Tailscale Serve setup's answer")
    }
}

/// `https://<dns_name>`, the machine's T3 over Tailscale Serve, if `dns_name` is a node of this
/// computer's own tailnet (`suffix`, its MagicDNS suffix). The machine reports the name and the
/// proxy sends its upstream token there, so nothing else passes: not another tailnet, not a
/// name outside `.ts.net`, not anything a URL parser could read as more than a host.
pub fn tailnet_origin(dns_name: &str, suffix: &str) -> Option<String> {
    let name = dns_name.trim().trim_end_matches('.').to_ascii_lowercase();
    let suffix = suffix.trim().trim_matches('.').to_ascii_lowercase();
    let label = name.strip_suffix(&format!(".{suffix}"))?;
    let ok = suffix.ends_with(".ts.net")
        && suffix.split('.').all(dns_label)
        && !label.contains('.')
        && dns_label(label);
    ok.then(|| format!("https://{name}"))
}

fn dns_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !label.starts_with('-')
        && !label.ends_with('-')
}

/// This computer's tailnet, as its own `tailscale status --json` says.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct LocalStatus {
    #[serde(rename = "BackendState", default)]
    pub backend_state: String,
    #[serde(rename = "MagicDNSSuffix", default)]
    pub magic_dns_suffix: String,
}

impl LocalStatus {
    /// The MagicDNS suffix of the tailnet this computer is on, if it is on one now.
    pub fn suffix(&self) -> Option<&str> {
        let s = self.magic_dns_suffix.trim().trim_matches('.');
        (self.backend_state == "Running" && !s.is_empty()).then_some(s)
    }
}

/// The Tailscale CLI on this computer: on PATH, or inside the macOS app.
fn local_cli() -> Option<std::path::PathBuf> {
    which::which("tailscale").ok().or_else(|| {
        let app = std::path::Path::new("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
        (cfg!(target_os = "macos") && app.exists()).then(|| app.to_owned())
    })
}

/// This computer's tailnet suffix, if Tailscale runs here. None when it is not installed, not
/// running, or does not answer: the machines are then reached over Boat, as without Tailscale.
pub async fn local_suffix() -> Option<String> {
    let cli = local_cli()?;
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::process::command(cli).args(["status", "--json", "--peers=false"]).output(),
    )
    .await
    .ok()?
    .ok()?;
    let status: LocalStatus = serde_json::from_slice(&out.stdout).ok()?;
    status.suffix().map(str::to_owned)
}

/// A link another device opens to pair T3 Code with the machine through the tailnet, as
/// `t3 pair` builds one: the one-time token in the fragment, never sent to a server.
pub fn pairing_url(origin: &str, credential: &str) -> Result<String> {
    let mut url = url::Url::parse(origin).context("the machine's tailnet address")?;
    url.set_path("/pair");
    url.set_query(None);
    let fragment: String = url::form_urlencoded::Serializer::new(String::new()).append_pair("token", credential).finish();
    url.set_fragment(Some(&fragment));
    Ok(url.into())
}

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
    fn serve_report_is_the_last_json_line() {
        let out = "Operator set\n{\"state\":\"on\",\"dnsName\":\"t3boat-dev.tail1234.ts.net\"}\n";
        assert_eq!(
            ServeReport::parse(out).unwrap(),
            ServeReport { state: ServeState::On, dns_name: "t3boat-dev.tail1234.ts.net".into() }
        );
        assert_eq!(ServeReport::parse(r#"{"state":"httpsOff","dnsName":"x"}"#).unwrap().state, ServeState::HttpsOff);
        assert_eq!(ServeReport::parse(r#"{"state":"offTailnet","dnsName":""}"#).unwrap().state, ServeState::OffTailnet);
        assert_eq!(ServeReport::parse(r#"{"state":"busy"}"#).unwrap().dns_name, "");
        assert!(ServeReport::parse(r#"{"state":"maybe"}"#).is_err());
        assert!(ServeReport::parse("tailscale: command not found").is_err());
    }

    #[test]
    fn serve_script_passes_the_restart_choice_first() {
        assert!(serve_script(true).starts_with("set -- 1\n"));
        assert!(serve_script(false).starts_with("set -- 0\n"));
        let s = serve_script(true);
        assert!(s.contains("T3CODE_TAILSCALE_SERVE=true"));
        assert!(s.contains("--operator="));
        assert!(!s.contains("tskey"));
    }

    #[test]
    fn only_a_node_of_this_computers_tailnet_is_a_tailnet_origin() {
        let suffix = "tail1234.ts.net";
        assert_eq!(
            tailnet_origin("t3boat-dev.tail1234.ts.net.", suffix).as_deref(),
            Some("https://t3boat-dev.tail1234.ts.net")
        );
        assert_eq!(tailnet_origin("T3boat-Dev.tail1234.ts.net", ".tail1234.ts.net.").as_deref(), Some("https://t3boat-dev.tail1234.ts.net"));
        for bad in [
            "t3boat-dev.tail9999.ts.net",           // another tailnet
            "t3boat-dev.evil.example",              // not the tailnet at all
            "evil.example/x.tail1234.ts.net",       // a path
            "evil.example#.tail1234.ts.net",        // a fragment
            "user@t3boat-dev.tail1234.ts.net",      // credentials
            "a.b.tail1234.ts.net",                  // a subdomain of a node
            "-dev.tail1234.ts.net",
            "t3boat-dev:8443.tail1234.ts.net",
            ".tail1234.ts.net",
            "tail1234.ts.net",
            "",
        ] {
            assert_eq!(tailnet_origin(bad, suffix), None, "{bad}");
        }
        // Not Tailscale's own DNS: never.
        assert_eq!(tailnet_origin("dev.corp.example", "corp.example"), None);
        assert_eq!(tailnet_origin("dev.tail1234.ts.net", ""), None);
    }

    #[test]
    fn local_suffix_needs_a_running_tailscale() {
        let up: LocalStatus =
            serde_json::from_str(r#"{"BackendState":"Running","MagicDNSSuffix":"tail1234.ts.net","Self":{}}"#).unwrap();
        assert_eq!(up.suffix(), Some("tail1234.ts.net"));
        let stopped: LocalStatus =
            serde_json::from_str(r#"{"BackendState":"Stopped","MagicDNSSuffix":"tail1234.ts.net"}"#).unwrap();
        assert_eq!(stopped.suffix(), None);
        let no_dns: LocalStatus = serde_json::from_str(r#"{"BackendState":"Running","MagicDNSSuffix":""}"#).unwrap();
        assert_eq!(no_dns.suffix(), None);
    }

    #[test]
    fn pairing_url_carries_the_token_in_the_fragment() {
        assert_eq!(
            pairing_url("https://t3boat-dev.tail1234.ts.net", "AbC-12_x").unwrap(),
            "https://t3boat-dev.tail1234.ts.net/pair#token=AbC-12_x"
        );
        assert_eq!(
            pairing_url("https://t3boat-dev.tail1234.ts.net", "a b&c").unwrap(),
            "https://t3boat-dev.tail1234.ts.net/pair#token=a+b%26c"
        );
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

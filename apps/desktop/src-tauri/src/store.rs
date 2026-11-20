//! Persistent app state: machines, settings and this month's spend, one JSON file in the app
//! data dir.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    /// Never changes: the id of the sandbox the machine was created on.
    pub id: String,
    /// Boat sandbox it runs on now, e.g. `bx_sad32u26`: another one after a restore.
    pub sandbox_id: String,
    pub name: String,
    pub size: String,
    /// Public HTTPS URL of the T3 server on the sandbox.
    pub upstream_url: String,
    /// Bearer token the proxy uses upstream. Never leaves this computer except to that server.
    pub upstream_token: String,
    /// Public HTTPS URL of the watchdog on the sandbox.
    pub watchdog_url: String,
    pub watchdog_secret: String,
    /// Loopback port the proxy serves this machine on. Stable, T3 Code stores it.
    pub local_port: u16,
    /// T3 server version installed on the machine.
    pub t3_version: String,
    /// `provision::revision()` of the scripts the machine runs.
    pub provision_revision: String,
    pub environment_id: String,
    /// Bearer tokens the proxy issued to T3 Code when it paired.
    pub client_tokens: Vec<String>,
    /// Bearer tokens the machine's server issued to T3 servers that linked to it through the
    /// proxy. The proxy only recognizes them: a call bearing one may wake the machine.
    pub link_tokens: Vec<LinkToken>,
    pub created_at: i64,
    /// When T3 on the machine next starts work on its own (an automation, a held webhook run, an
    /// agent resuming after a usage limit), as its watchdog last said: ms since epoch.
    #[serde(default)]
    pub next_due_at: Option<i64>,
    /// T3 Connect is on there: other devices reach it with the user's T3 account.
    #[serde(default)]
    pub connect: bool,
    /// Joined the user's tailnet when created: a delete logs it out first.
    #[serde(default)]
    pub tailscale: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkToken {
    pub token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub start_at_login: bool,
    /// How machines join the user's tailnet. A state file from before it existed, with a key
    /// saved, reads as `Key`: see `Store::load`.
    pub tailscale: TailscaleMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self { start_at_login: true, tailscale: TailscaleMode::Off }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TailscaleMode {
    /// Machines stay off the tailnet.
    #[default]
    Off,
    /// Each new machine opens Tailscale's sign-in page here and joins as the user who signs in.
    Browser,
    /// New machines join with the auth key saved in the keychain, during their create.
    Key,
}

/// One sandbox's spend as last looked up. Kept after the sandbox is gone (its machine deleted, or
/// a restore replaced it) so the totals still count it until its month is over.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    /// Id of the machine that ran on the sandbox.
    pub machine: String,
    /// Start of the local day of the lookup, unix seconds.
    pub day: i64,
    /// Dollars since `day`.
    pub today: f64,
    pub awake_secs_today: u64,
    /// Dollars since the start of `day`'s month.
    pub month: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    pub machines: Vec<Machine>,
    pub settings: Settings,
    /// By sandbox id.
    pub spend: HashMap<String, Spend>,
    /// Local ports of deleted machines, never given out again: T3 Code may still list one (it
    /// cannot be told to forget a backend), and would reach a new machine there as the old.
    pub retired_ports: Vec<u16>,
    /// Sandboxes of creates still running: deleted at the next start if the app quit or crashed
    /// before the create finished.
    pub pending_sandboxes: Vec<String>,
    /// A Tailscale auth key is in the keychain. Only this flag is kept here, never the key, so
    /// that without one nothing reads the keychain at all.
    pub tailscale_key: bool,
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("state.json"),
        }
    }

    pub fn load(&self) -> Result<State> {
        match std::fs::read(&self.path) {
            Ok(bytes) => parse(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// The file holds bearer tokens.
    pub fn save(&self, state: &State) -> Result<()> {
        write_private(&self.path, &serde_json::to_vec_pretty(state)?)
    }
}

/// A key saved before the Tailscale mode existed keeps working: the mode is then "key".
fn parse(bytes: &[u8]) -> Result<State> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let had_mode = value["settings"].get("tailscale").is_some();
    let mut state: State = serde_json::from_value(value)?;
    if !had_mode && state.tailscale_key {
        state.settings.tailscale = TailscaleMode::Key;
    }
    Ok(state)
}

/// Atomic write of a file only its owner can read, never readable by others even briefly.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().expect("path has a parent"))?;
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&tmp)?.write_all(bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Removes everything in `dir` but `keep`, whatever an interrupted write left behind included.
pub fn sweep(dir: &Path, keep: &HashSet<PathBuf>) {
    let entries = std::fs::read_dir(dir).into_iter().flatten().flatten();
    for path in entries.map(|e| e.path()).filter(|p| !keep.contains(p)) {
        let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, TailscaleMode};

    #[test]
    fn a_saved_key_from_before_the_mode_means_key() {
        let old = br#"{"settings":{"startAtLogin":true},"tailscaleKey":true}"#;
        assert_eq!(parse(old).unwrap().settings.tailscale, TailscaleMode::Key);
        let no_key = br#"{"settings":{"startAtLogin":false}}"#;
        assert_eq!(parse(no_key).unwrap().settings.tailscale, TailscaleMode::Off);
        // Chosen since: kept, the key notwithstanding.
        let chosen = br#"{"settings":{"tailscale":"browser"},"tailscaleKey":true}"#;
        assert_eq!(parse(chosen).unwrap().settings.tailscale, TailscaleMode::Browser);
        assert_eq!(parse(b"{}").unwrap().settings.tailscale, TailscaleMode::Off);
    }
}

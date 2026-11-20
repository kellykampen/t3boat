//! Creating and upgrading machines: everything that runs on the sandbox goes through
//! `provision.sh`, which is idempotent.

use crate::boat::Boat;
use crate::proxy::auth::random_token;
use crate::store::Machine;
use crate::tailscale::AuthKey;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde::Deserialize;

const PROVISION_SH: &str = include_str!("provision.sh");
const WATCHDOG_MJS: &str = include_str!("watchdog.mjs");
const T3_PORT: u16 = 3773;
const WATCHDOG_PORT: u16 = 3774;

/// Identifies the provisioning scripts; a machine provisioned with others gets them on wake.
pub fn revision() -> String {
    let hash = fnv1a(PROVISION_SH.bytes().chain(WATCHDOG_MJS.bytes()).chain(SLEEP_AFTER_SECS.to_string().bytes()));
    format!("{hash:016x}")
}

/// FNV-1a: stable across builds, unlike std's hasher.
pub fn fnv1a(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provisioned {
    pub environment_id: String,
    pub upstream_token: Option<String>,
}

/// Machine sizes, as Boat sells them.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeInfo {
    pub id: &'static str,
    pub spec: &'static str,
    pub hourly: f64,
}

pub const SIZES: &[SizeInfo] = &[
    SizeInfo { id: "small", spec: "2 vCPU · 4 GB", hourly: 0.018 },
    SizeInfo { id: "default", spec: "4 vCPU · 8 GB", hourly: 0.036 },
    SizeInfo { id: "large", spec: "8 vCPU · 16 GB", hourly: 0.072 },
];

pub fn size(id: &str) -> Result<&'static SizeInfo> {
    SIZES.iter().find(|s| s.id == id).ok_or_else(|| anyhow!("unknown size {id}"))
}

/// How long a machine stays up after the last agent turn and the last action through t3boat.
/// Long enough to read a result and type a follow-up without waiting for a wake; idle minutes
/// cost under a cent even on the large size.
pub const SLEEP_AFTER_SECS: u64 = 300;

/// The machine's sandbox, as Boat lists it.
pub fn sandbox_name(name: &str) -> String {
    format!("t3boat · {name}")
}

/// The saved snapshot the watchdog replaces at every sleep. Boat deletes a stopped sandbox's
/// disk after 14 days; this stays, so the machine can be restored from it. Unique per machine,
/// whoever else uses the Boat account. Boat takes lowercase letters, digits and dashes.
pub fn snapshot_name(name: &str, machine_id: &str) -> String {
    let id: String = machine_id.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    format!("t3boat-{name}-{id}")
}

/// A snapshot to start a new machine from, as the user gives it: a saved snapshot's name
/// (lowercase letters, digits and dashes, as Boat names them) or a Boat share code (`bsh_…`).
/// Checked before it reaches Boat's command line.
pub fn check_start_snapshot(s: &str) -> Result<()> {
    // Not starting with a dash: Boat's command line would read it as an option.
    let name = s.len() <= 128
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let share = s
        .strip_prefix("bsh_")
        .is_some_and(|code| !code.is_empty() && code.len() <= 128 && code.chars().all(|c| c.is_ascii_alphanumeric()));
    if !name && !share {
        bail!("\"{s}\" is not a snapshot name (lowercase letters, digits and dashes, starting with a letter or digit) or a Boat share code (bsh_…)");
    }
    Ok(())
}

/// Copies the scripts over as a file (the watchdog secret and the Tailscale key must not appear
/// in any command line) and runs `provision.sh`. `tailscale`: join the user's tailnet with this
/// key; only a create passes one, so an upgrade never logs a machine in or needs the key. The
/// key reaches `provision.sh` as a 0600 file in the script's temp dir, deleted on exit.
/// `fresh`: the disk came from someone else's snapshot (possibly another t3boat machine's), so
/// its T3 and watchdog identity is wiped before anything else.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    boat: &Boat,
    sandbox_id: &str,
    name: &str,
    t3_version: &str,
    watchdog_secret: &str,
    snapshot: &str,
    mint_upstream_token: bool,
    tailscale: Option<&AuthKey>,
    fresh: bool,
) -> Result<Provisioned> {
    let script = script(name, t3_version, watchdog_secret, snapshot, mint_upstream_token, tailscale, fresh)?;
    let name = format!("t3boat-provision-{}.sh", random_token());
    let local = std::env::temp_dir().join(&name);
    crate::store::write_private(&local, script.as_bytes())?;
    let remote = format!("/tmp/{name}");
    let uploaded = boat.upload(sandbox_id, &local, &remote).await;
    let _ = std::fs::remove_file(&local);
    uploaded?;
    let out = boat.exec_ok(sandbox_id, &format!("bash {remote}"), 600).await?;
    let line = out
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .ok_or_else(|| anyhow!("provisioning printed no result"))?;
    Ok(serde_json::from_str(line)?)
}

/// The uploaded script: unpacks `provision.sh` and `watchdog.mjs` (and the Tailscale key, as a
/// 0600 file) into a temp dir it deletes on exit, with itself, and runs `provision.sh`.
fn script(
    name: &str,
    t3_version: &str,
    watchdog_secret: &str,
    snapshot: &str,
    mint_upstream_token: bool,
    tailscale: Option<&AuthKey>,
    fresh: bool,
) -> Result<String> {
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
    let (ts_file, ts_env) = match tailscale {
        Some(key) => (
            format!("(umask 077; echo {} | base64 -d > \"$d/tailscale.key\")\n", b64(key.expose())),
            "T3BOAT_TAILSCALE_KEY_FILE=\"$d/tailscale.key\" ",
        ),
        None => (String::new(), ""),
    };
    Ok(format!(
        "set -e; d=$(mktemp -d); trap 'rm -rf \"$d\" \"$0\"' EXIT\n\
         echo {p} | base64 -d > \"$d/provision.sh\"; echo {w} | base64 -d > \"$d/watchdog.mjs\"\n\
         {ts_file}\
         T3BOAT_SECRET={s} {ts_env}bash \"$d/provision.sh\" {v} {sleep} {snapshot} {mint} {name} {fresh}\n",
        p = b64(PROVISION_SH),
        w = b64(WATCHDOG_MJS),
        v = shell_word(t3_version)?,
        s = shell_word(watchdog_secret)?,
        sleep = SLEEP_AFTER_SECS,
        snapshot = shell_word(snapshot)?,
        mint = u8::from(mint_upstream_token),
        name = shell_word(name)?,
        fresh = u8::from(fresh),
    ))
}

/// Only `[A-Za-z0-9._-]` reaches the remote shell.
pub(crate) fn shell_word(s: &str) -> Result<&str> {
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        bail!("unsafe value for the remote shell: {s}");
    }
    Ok(s)
}

/// Hosts the T3 server's and the watchdog's ports publicly: their URLs, in that order.
/// Idempotent: hosting again keeps the URLs.
pub async fn host(boat: &Boat, sandbox_id: &str) -> Result<(String, String)> {
    tokio::try_join!(boat.host_public(sandbox_id, T3_PORT), boat.host_public(sandbox_id, WATCHDOG_PORT))
}

/// Creates a new machine end to end. Takes about a minute, most of it Boat and the T3 install.
/// On failure after `created`, the caller deletes the sandbox. `start_from`: a saved snapshot or
/// share code its disk starts from instead of Boat's base image; it gets its own identity.
/// `tailscale`: also joins the user's tailnet; a failure there fails the create.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    boat: &Boat,
    name: &str,
    size: &str,
    start_from: Option<&str>,
    t3_version: &str,
    local_port: u16,
    tailscale: Option<&AuthKey>,
    created: impl FnOnce(&str),
    progress: impl Fn(&str),
) -> Result<Machine> {
    self::size(size)?;
    if let Some(s) = start_from {
        check_start_snapshot(s)?;
    }
    progress("Creating sandbox on Boat");
    let id = boat
        .create(&sandbox_name(name), size, start_from)
        .await
        .context("creating the Boat sandbox")?;
    created(&id);
    progress(if tailscale.is_some() { "Installing T3 Code and Tailscale" } else { "Installing T3 Code" });
    let secret = random_token();
    let provisioned =
        run(boat, &id, name, t3_version, &secret, &snapshot_name(name, &id), true, tailscale, start_from.is_some()).await?;
    progress("Opening the machine's address");
    let (upstream_url, watchdog_url) = host(boat, &id).await?;
    Ok(Machine {
        id: id.clone(),
        sandbox_id: id,
        name: name.to_owned(),
        size: size.to_owned(),
        upstream_url,
        upstream_token: provisioned
            .upstream_token
            .ok_or_else(|| anyhow!("provisioning issued no token"))?,
        watchdog_url,
        watchdog_secret: secret,
        local_port,
        t3_version: t3_version.to_owned(),
        provision_revision: revision(),
        environment_id: provisioned.environment_id,
        client_tokens: Vec::new(),
        link_tokens: Vec::new(),
        created_at: chrono::Utc::now().timestamp(),
        next_due_at: None,
        connect: false,
        tailscale: tailscale.is_some(),
        tailnet_url: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "tskey-auth-kAbc123CNTRL-secretpart";

    #[test]
    fn tailscale_key_travels_as_a_file() {
        let key = AuthKey::parse(KEY).unwrap();
        let s = script("dev", "0.0.21", "secret", "t3boat-dev-bx", true, Some(&key), false).unwrap();
        assert!(!s.contains(KEY), "the key must not appear in the clear");
        assert!(s.contains(&base64::engine::general_purpose::STANDARD.encode(KEY)));
        assert!(s.contains("(umask 077; echo "));
        assert!(s.contains("T3BOAT_TAILSCALE_KEY_FILE=\"$d/tailscale.key\" bash"));
    }

    #[test]
    fn no_tailscale_without_a_key() {
        let s = script("dev", "0.0.21", "secret", "t3boat-dev-bx", false, None, false).unwrap();
        assert!(!s.contains("tailscale.key") && !s.contains("T3BOAT_TAILSCALE"));
    }

    #[test]
    fn start_snapshot() {
        for ok in ["base-tools", "t3boat-dev-sb-1a2b", "bsh_AbC123"] {
            assert!(check_start_snapshot(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Tools", "a b", "x;rm -rf ~", "--help", "bsh_", "bsh_a-b", "snap_1", "$(id)"] {
            assert!(check_start_snapshot(bad).is_err(), "{bad}");
        }
    }
}

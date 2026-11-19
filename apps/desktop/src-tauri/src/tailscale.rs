//! The user's Tailscale auth key: new machines join their tailnet with it, with Tailscale SSH.
//! It lives in the OS credential store (the macOS Keychain) and nowhere else on this computer:
//! not the state file, not logs, not error messages. A machine gets it as a file inside the
//! provisioning upload (`provision::run`), never on a command line.

use anyhow::{anyhow, bail, Result};

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
/// node t3boat joined (it left the owner marker) is touched.
pub const LOGOUT_SCRIPT: &str =
    "if [ -f ~/.t3boat/tailscale-owner ] && command -v tailscale >/dev/null; then sudo tailscale logout; fi";

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
}

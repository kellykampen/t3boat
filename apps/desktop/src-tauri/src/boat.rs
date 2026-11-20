//! Thin wrapper over the user's own Boat CLI. Uses its existing login; never stores Boat credentials.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use crate::process::command;

#[derive(Clone, Debug)]
pub struct Boat {
    bin: PathBuf,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Sandbox {
    pub id: String,
    pub state: String,
    pub stopped_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub identifier: Option<String>,
    pub signed_in: bool,
    pub plan: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub dollars: f64,
    pub seconds: u64,
}

/// A saved (named) snapshot a new machine can start from.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// The sandbox it was taken of; Boat may not say.
    #[serde(default)]
    pub source_sandbox_id: Option<String>,
    pub status: String,
}

/// A pending browser sign-in: the page that completes it, and the code that page may ask for.
#[derive(Clone, Debug)]
pub struct SignIn {
    pub url: String,
    pub code: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    Resumed,
    /// Boat deleted the stopped sandbox's disk: it keeps one 14 days.
    DataExpired,
}

#[derive(Clone, Debug, Default)]
pub struct Starts {
    pub used_today: u64,
    pub limit_today: u64,
}

impl Boat {
    /// Finds the CLI the Boat installer puts in `~/.ascii/bin`, else on PATH.
    pub fn locate() -> Option<Self> {
        let exe = if cfg!(windows) { "boat.exe" } else { "boat" };
        let installed = dirs::home_dir().map(|h| h.join(".ascii").join("bin").join(exe));
        installed
            .filter(|p| p.is_file())
            .or_else(|| which::which("boat").ok())
            .map(|bin| Self { bin })
    }

    /// Runs the CLI; returns whether it succeeded, stdout and stderr.
    async fn output(&self, args: &[&str], timeout: Duration) -> Result<(bool, String, String)> {
        let mut cmd = command(&self.bin);
        // `--json` first: anything after `--` belongs to the remote command, and a nested
        // subcommand (`snapshot rm`) is only read as one with no option before it.
        cmd.arg("--json")
            .args(args)
            .env("BOAT_NO_UPDATE", "1")
            .kill_on_drop(true);
        // Only the subcommand goes into errors: arguments can hold scripts.
        let sub = args.first().copied().unwrap_or_default();
        let out = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| anyhow!("boat {sub} timed out"))?
            .with_context(|| format!("running boat {sub}"))?;
        Ok((
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }

    async fn run(&self, args: &[&str], timeout: Duration) -> Result<String> {
        let (ok, stdout, stderr) = self.output(args, timeout).await?;
        if !ok {
            let sub = args.first().copied().unwrap_or_default();
            bail!("boat {sub} failed: {}", said(&stdout, &stderr));
        }
        Ok(stdout)
    }

    /// The CLI prints JSON lines for long operations; the last line is the result.
    fn last_json(out: &str) -> Result<Value> {
        let line = out
            .lines()
            .rev()
            .find(|l| l.trim_start().starts_with('{'))
            .ok_or_else(|| anyhow!("boat printed no JSON: {out}"))?;
        Ok(serde_json::from_str(line)?)
    }

    pub async fn account(&self) -> Result<Account> {
        let v = Self::last_json(&self.run(&["status"], Duration::from_secs(20)).await?)?;
        Ok(serde_json::from_value(v["account"].clone())?)
    }

    /// Returns the new sandbox's id. `snapshot`: the saved snapshot its disk starts from.
    pub async fn create(&self, name: &str, size: &str, snapshot: Option<&str>) -> Result<String> {
        let mut args = vec!["new", "--name", name, "--type", size, "--no-auto-stop"];
        args.extend(snapshot.into_iter().flat_map(|s| ["--from", s]));
        let out = self.run(&args, Duration::from_secs(300)).await?;
        let v = Self::last_json(&out)?;
        let id = v["id"].as_str().ok_or_else(|| anyhow!("boat new returned no id"))?;
        Ok(id.to_owned())
    }

    pub async fn info(&self, id: &str) -> Result<Sandbox> {
        let v = Self::last_json(&self.run(&["info", id], Duration::from_secs(20)).await?)?;
        Ok(serde_json::from_value(v["sandbox"].clone())?)
    }

    pub async fn list(&self) -> Result<Vec<Sandbox>> {
        let v = Self::last_json(&self.run(&["list", "--all"], Duration::from_secs(20)).await?)?;
        Ok(serde_json::from_value(v["sandboxes"].clone())?)
    }

    /// Gone only when Boat's own list confirms it: one bad answer must not retire a machine.
    pub async fn confirmed_gone(&self, id: &str) -> bool {
        matches!(self.list().await, Ok(l) if l.iter().all(|s| s.id != id))
    }

    /// Resumes onto `size`, which is how a machine changes size.
    pub async fn resume(&self, id: &str, size: &str) -> Result<Resume> {
        let (ok, stdout, stderr) =
            self.output(&["resume", id, "--no-auto-stop", "--type", size], Duration::from_secs(300)).await?;
        let said = said(&stdout, &stderr);
        match ok {
            true => Ok(Resume::Resumed),
            false if said.contains("snapshot_data_expired") => Ok(Resume::DataExpired),
            false => bail!("boat resume failed: {said}"),
        }
    }

    /// The saved snapshots a machine can start from: ready ones only, newest first.
    pub async fn snapshots(&self) -> Result<Vec<Snapshot>> {
        parse_snapshots(&self.run(&["snapshots", "--all"], Duration::from_secs(30)).await?)
    }

    pub async fn delete_snapshot(&self, name: &str) -> Result<()> {
        self.run(&["snapshot", "rm", name], Duration::from_secs(60)).await.map(drop)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        self.run(&["delete", id, "--yes"], Duration::from_secs(120))
            .await
            .map(drop)
    }

    /// Exposes `port` (bound to loopback on the sandbox) on a stable public HTTPS URL.
    pub async fn host_public(&self, id: &str, port: u16) -> Result<String> {
        let port = port.to_string();
        let v = Self::last_json(
            &self
                .run(&["host", id, &port, "--public", "--localhost"], Duration::from_secs(60))
                .await?,
        )?;
        v["url"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("boat host returned no url"))
    }

    pub async fn exec(&self, id: &str, script: &str, timeout_secs: u64) -> Result<ExecResult> {
        let t = timeout_secs.to_string();
        // The CLI exits with the command's code; its JSON result is what matters.
        let (_, stdout, stderr) = self
            .output(
                &["exec", id, "--timeout", &t, "--", "bash", "-lc", script],
                Duration::from_secs(timeout_secs + 60),
            )
            .await?;
        match Self::last_json(&stdout) {
            Ok(v) => Ok(serde_json::from_value(v)?),
            Err(_) => bail!("boat exec failed: {}", stderr.trim()),
        }
    }

    /// Runs `script` and fails unless it exits 0. Returns stdout.
    pub async fn exec_ok(&self, id: &str, script: &str, timeout_secs: u64) -> Result<String> {
        let r = self.exec(id, script, timeout_secs).await?;
        if r.exit_code != Some(0) {
            let detail = if r.stderr.trim().is_empty() { r.stdout.trim() } else { r.stderr.trim() };
            bail!("command on the machine failed ({:?}): {detail}", r.exit_code);
        }
        Ok(r.stdout)
    }

    pub async fn usage_since(&self, id: &str, since_unix: i64) -> Result<Usage> {
        let since = since_unix.to_string();
        let v = Self::last_json(
            &self.run(&["usage", id, "--since", &since], Duration::from_secs(20)).await?,
        )?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn starts(&self) -> Result<Starts> {
        let v = Self::last_json(&self.run(&["limits", "--all"], Duration::from_secs(20)).await?)?;
        let day = &v["starts"]["day"];
        Ok(Starts {
            used_today: day["used"].as_u64().unwrap_or(0),
            limit_today: day["limit"].as_u64().unwrap_or(0),
        })
    }

    /// Starts a browser sign-in, or picks up the one already pending. Without a terminal the
    /// CLI opens no browser, so the caller opens the page.
    pub async fn start_sign_in(&self) -> Result<SignIn> {
        let v = Self::last_json(&self.run(&["onboard"], Duration::from_secs(60)).await?)?;
        let url = v["url"].as_str().ok_or_else(|| anyhow!("boat onboard gave no sign-in page: {v}"))?;
        Ok(SignIn { url: url.to_owned(), code: v["userCode"].as_str().map(str::to_owned) })
    }

    /// Waits until the started sign-in completes or expires.
    pub async fn finish_sign_in(&self) -> Result<()> {
        self.run(&["onboard", "--wait"], Duration::from_secs(900)).await.map(drop)
    }

    /// Copies a local file to `remote_path` on the sandbox.
    pub async fn upload(&self, id: &str, local: &Path, remote_path: &str) -> Result<()> {
        let to = format!("{id}:{remote_path}");
        self.run(&["scp", path_str(local)?, &to], Duration::from_secs(1800)).await.map(drop)
    }

    /// Forwards `port` on this computer to the same port in the sandbox, for as long as the
    /// returned process lives.
    pub async fn forward(&self, id: &str, port: u16) -> Result<tokio::process::Child> {
        // Else another listener on the port would pass the readiness check below.
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
            bail!("port {port} is in use on this computer; finish or close the other sign-in first");
        }
        let p = port.to_string();
        let mut child = command(&self.bin)
            .args(["forward", id, "--remote", &p, "--local", &p])
            .env("BOAT_NO_UPDATE", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("running boat forward")?;
        for _ in 0..100 {
            if let Some(status) = child.try_wait()? {
                bail!("boat forward exited ({status}); is port {port} free on this computer?");
            }
            if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                return Ok(child);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("boat forward did not open port {port}")
    }
}

/// `boat snapshots --all` output: the `named` arrays of every JSON line (one per page, should
/// the CLI print pages separately), ready ones only, each name once, newest first. An item Boat
/// words differently is skipped rather than failing the whole list.
fn parse_snapshots(out: &str) -> Result<Vec<Snapshot>> {
    let pages: Vec<Value> = out
        .lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    if pages.is_empty() {
        bail!("boat printed no JSON: {out}");
    }
    let mut list: Vec<Snapshot> = pages
        .iter()
        .filter_map(|p| p["named"].as_array())
        .flatten()
        .filter_map(|v| serde_json::from_value::<Snapshot>(v.clone()).ok())
        .filter(|s| s.status == "ready")
        .collect();
    list.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    let mut seen = std::collections::HashSet::new();
    list.retain(|s| seen.insert(s.name.clone()));
    Ok(list)
}

/// What a failed command said: its stderr, else its stdout.
fn said<'a>(stdout: &'a str, stderr: &'a str) -> &'a str {
    if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() }
}

/// Command-line form of a path; the Boat CLI and git only take UTF-8 here.
pub(crate) fn path_str(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| anyhow!("path is not UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_ready_named_snapshots_newest_first() {
        let page1 = r#"{"named":[{"createdAt":"2026-11-19T04:18:16.029Z","name":"t3boat-skiff-bx-xnfee34h","snapshotId":"03e5","sourceSandboxId":"bx_xnfee34h","status":"ready","type":"default"},{"createdAt":"2026-11-10T09:00:00Z","name":"base-tools","snapshotId":"a1","sourceSandboxId":"bx_old","status":"ready","type":"default"},{"createdAt":"2026-11-20T09:00:00Z","name":"half-done","snapshotId":"a2","sourceSandboxId":"bx_new","status":"pending","type":"default"},{"name":"no-date","status":"ready"}],"pageInfo":{"hasMore":true},"snapshots":[{"id":"x","status":"completed"}]}"#;
        let page2 = r#"{"named":[{"createdAt":"2026-11-18T12:00:00Z","name":"node-22-stack","snapshotId":"b1","status":"ready","type":"default"},{"createdAt":"2026-11-19T04:18:16.029Z","name":"t3boat-skiff-bx-xnfee34h","snapshotId":"03e5","sourceSandboxId":"bx_xnfee34h","status":"ready","type":"default"}],"pageInfo":{"hasMore":false}}"#;
        let out = format!("warning: something\n{page1}\n{page2}\n");
        let list = parse_snapshots(&out).unwrap();
        let names: Vec<_> = list.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["t3boat-skiff-bx-xnfee34h", "node-22-stack", "base-tools"]);
        assert_eq!(list[0].source_sandbox_id.as_deref(), Some("bx_xnfee34h"));
        assert_eq!(list[1].source_sandbox_id, None);

        let json = serde_json::to_value(&list[0]).unwrap();
        assert_eq!(json["createdAt"], "2026-11-19T04:18:16.029Z");
        assert_eq!(json["sourceSandboxId"], "bx_xnfee34h");
        assert_eq!(json["status"], "ready");
    }

    #[test]
    fn snapshots_without_any_or_without_json() {
        assert!(parse_snapshots(r#"{"named":[],"snapshots":[]}"#).unwrap().is_empty());
        assert!(parse_snapshots(r#"{"snapshots":[]}"#).unwrap().is_empty());
        assert!(parse_snapshots("not signed in").is_err());
    }
}

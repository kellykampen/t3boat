//! Joining the user's tailnet by signing in with the browser, as Tailscale's own CLI does: an
//! interactive `tailscale up` runs on the machine, its sign-in page opens here, and the node
//! joins as whoever signs in. No key ever exists for it. Works on any machine, old ones too.

use super::{provision, MachineRuntime, Power};
use crate::tailscale::{self, Status};
use anyhow::{bail, Result};
use serde::Serialize;
use std::time::{Duration, Instant};

/// Tailscale's sign-in links last about this long; `tailscale up` itself gives up after 15m.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_secs(3);

/// A join in progress, or how the last one failed: `MachineView::tailscale_join`.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum TailnetJoin {
    /// Waking the machine, installing Tailscale, starting `tailscale up`.
    Starting,
    /// The sign-in page is open in the browser. `url`: none while a tailnet admin has to approve
    /// the device instead.
    WaitingForSignIn { url: Option<String> },
    Error { message: String },
}

impl MachineRuntime {
    pub fn tailnet_join(&self) -> Option<TailnetJoin> {
        self.tailnet_join.lock().unwrap().clone()
    }

    fn set_tailnet_join(&self, join: Option<TailnetJoin>) {
        *self.tailnet_join.lock().unwrap() = join;
        self.shared.changed();
    }

    /// The sign-in page of the join waiting for one, if any.
    pub fn tailnet_sign_in_url(&self) -> Option<String> {
        match self.tailnet_join() {
            Some(TailnetJoin::WaitingForSignIn { url }) => url,
            _ => None,
        }
    }

    /// Ends a join in progress; the `tailscale up` on the machine stops too.
    pub fn cancel_tailnet_join(&self) {
        self.tailnet_cancel.notify_waiters();
    }

    /// Joins the user's tailnet as `t3boat-<name>` with Tailscale SSH: true once on it, false when
    /// cancelled. Wakes the machine, and keeps it up while the user is in the browser.
    pub async fn join_tailnet(&self) -> Result<bool> {
        let Ok(_joining) = self.joining_tailnet.try_lock() else {
            bail!("already connecting to your tailnet; finish the sign-in in the browser");
        };
        let cancelled = self.tailnet_cancel.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        self.set_tailnet_join(Some(TailnetJoin::Starting));
        let result = tokio::select! {
            r = self.join_tailnet_now() => r.map(|()| true),
            () = &mut cancelled => Ok(false),
        };
        if !matches!(result, Ok(true)) && self.power() == Power::Running {
            // Best effort: a sign-in left waiting would join the machine later, unrecorded.
            let _ = self.shared.boat.exec(&self.sandbox_id(), tailscale::STOP_SIGN_IN_SCRIPT, 30).await;
        }
        self.set_tailnet_join(match &result {
            Err(e) => Some(TailnetJoin::Error { message: format!("{e:#}") }),
            Ok(_) => None,
        });
        result
    }

    async fn join_tailnet_now(&self) -> Result<()> {
        self.ensure_awake().await?;
        self.kept_awake(async {
            let record = self.record();
            let snapshot = provision::snapshot_name(&record.name, &record.id);
            let script = tailscale::connect_script(&record.name, &snapshot)?;
            let sandbox = self.sandbox_id();
            let boat = &self.shared.boat;
            let mut status = Status::parse(&boat.exec_ok(&sandbox, &script, 300).await?)?;
            let started = Instant::now();
            let mut opened = String::new();
            while !status.running() {
                if let Some(url) = status.sign_in_url().filter(|u| *u != opened) {
                    opened = url.to_owned();
                    self.set_tailnet_join(Some(TailnetJoin::WaitingForSignIn { url: Some(opened.clone()) }));
                    crate::app::open_url(&opened)?;
                } else if status.needs_approval() && opened.is_empty() {
                    self.set_tailnet_join(Some(TailnetJoin::WaitingForSignIn { url: None }));
                }
                if started.elapsed() > SIGN_IN_TIMEOUT {
                    bail!(if status.needs_approval() {
                        "the machine is waiting for a tailnet admin to approve it in Tailscale's admin console"
                    } else {
                        "the Tailscale sign-in was not finished in the browser"
                    });
                }
                tokio::time::sleep(POLL).await;
                // One failed look (a network blip) is not the end; a machine that went away is.
                match boat.exec_ok(&sandbox, tailscale::STATUS_COMMAND, 30).await.and_then(|o| Status::parse(&o)) {
                    Ok(s) => status = s,
                    Err(e) if self.power() == Power::Running => tracing::warn!("Tailscale status of {}: {e:#}", record.id),
                    Err(e) => return Err(e),
                }
            }
            boat.exec_ok(&sandbox, &tailscale::owner_script(&snapshot)?, 30).await?;
            self.update_record(|r| r.tailscale = true);
            Ok(())
        })
        .await
    }
}

//! Joining the user's tailnet by signing in with the browser, as Tailscale's own CLI does: an
//! interactive `tailscale up` runs on the machine, its sign-in page opens here, and the node
//! joins as whoever signs in. No key ever exists for it. Works on any machine, old ones too.
//!
//! Once on the tailnet, T3 there publishes itself over Tailscale Serve HTTPS with its own
//! `T3CODE_TAILSCALE_SERVE` (`ensure_tailnet_serve`), and the proxy reaches it there while it
//! answers there (`tailnet_route`), over Boat's public URL otherwise.

use super::{provision, MachineRuntime, Power};
use crate::tailscale::{self, ServeReport, ServeState, Status};
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::Value;
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
            // Extra: the machine is on the tailnet whether or not this works; a wake retries.
            if let Err(e) = self.ensure_tailnet_serve().await {
                tracing::warn!("Tailscale Serve on {}: {e:#}", record.id);
            }
            Ok(())
        })
        .await
    }
}

/// The tailnet route is checked this often while the machine runs, and right after it failed.
const ROUTE_RECHECK: Duration = Duration::from_secs(30);
/// A good check counts this long: a check missed (the run loop busy) does not flip the route.
const ROUTE_VALID: Duration = Duration::from_secs(75);
/// T3 answers its descriptor at once; a tailnet that needs longer is not worth taking.
const ROUTE_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The last check of the tailnet route: whether T3 answered there as this machine, and when.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RouteCheck {
    pub ok: bool,
    pub at: Option<Instant>,
}

impl RouteCheck {
    /// The proxy takes the tailnet route: the machine runs, T3 is published there, and answered
    /// there lately.
    fn usable(&self, running: bool, published: bool, now: Instant) -> bool {
        running && published && self.ok && self.at.is_some_and(|t| now.saturating_duration_since(t) < ROUTE_VALID)
    }

    /// Time to check again: never checked since the machine came up or the route failed, or
    /// not for a while.
    fn due(&self, running: bool, published: bool, now: Instant) -> bool {
        running && published && self.at.is_none_or(|t| now.saturating_duration_since(t) >= ROUTE_RECHECK)
    }
}

/// What a `ServeReport` changes: the tailnet address to keep (`Some(None)` clears it, `None`
/// leaves it as it is), and whether HTTPS certificates are off. `suffix`: this computer's
/// tailnet; without it (Tailscale off here) a name cannot be checked, so none is taken.
fn serve_outcome(report: &ServeReport, suffix: Option<&str>) -> (Option<Option<String>>, bool) {
    match report.state {
        ServeState::On | ServeState::Starting => {
            (suffix.map(|s| tailscale::tailnet_origin(&report.dns_name, s)), false)
        }
        ServeState::Busy => (None, false),
        ServeState::HttpsOff => (Some(None), true),
        ServeState::OffTailnet => (Some(None), false),
    }
}

impl MachineRuntime {
    /// Its T3 over Tailscale Serve, when the proxy should reach it there now.
    pub fn tailnet_route(&self) -> Option<String> {
        let url = self.record().tailnet_url?;
        let check = *self.tailnet_check.lock().unwrap();
        check.usable(self.power() == Power::Running, true, Instant::now()).then_some(url)
    }

    /// Where the proxy reaches T3 on the machine now: the tailnet, else Boat's public URL. The
    /// same server either way, so the same token.
    pub fn route_url(&self) -> String {
        self.tailnet_route().unwrap_or_else(|| self.upstream_url())
    }

    /// A request over the tailnet failed: Boat's URL from now on, until a check finds the
    /// tailnet answering again.
    pub fn tailnet_failed(&self) {
        let was = self.tailnet_route().is_some();
        *self.tailnet_check.lock().unwrap() = RouteCheck { ok: false, at: None };
        tracing::info!("{}: tailnet route failed, using Boat's URL", self.id());
        if was {
            self.shared.changed();
        }
    }

    /// Forgets the last check: the machine stopped, or came up again.
    pub(super) fn reset_tailnet_check(&self) {
        *self.tailnet_check.lock().unwrap() = RouteCheck::default();
    }

    /// HTTPS certificates are off on the tailnet, as last seen: Tailscale Serve cannot work.
    pub fn tailnet_https_off(&self) -> bool {
        *self.tailnet_https_off.lock().unwrap()
    }

    /// Checks the tailnet route if due, from the run loop: the proxy itself never waits on it.
    pub(super) async fn check_tailnet_route(&self) {
        let Some(url) = self.record().tailnet_url else { return };
        let running = self.power() == Power::Running;
        if !self.tailnet_check.lock().unwrap().due(running, true, Instant::now()) {
            return;
        }
        let was = self.tailnet_route().is_some();
        let ok = self.answers_at(&url).await;
        *self.tailnet_check.lock().unwrap() = RouteCheck { ok, at: Some(Instant::now()) };
        if was != self.tailnet_route().is_some() {
            tracing::info!("{}: {}", self.id(), if ok { "reached over the tailnet" } else { "tailnet route down, using Boat's URL" });
            self.shared.changed();
        }
    }

    /// T3 answers at `origin` as this machine. Also false when this computer is off the tailnet.
    async fn answers_at(&self, origin: &str) -> bool {
        let url = format!("{origin}/.well-known/t3/environment");
        let res = match self.shared.http.get(url).timeout(ROUTE_PROBE_TIMEOUT).send().await {
            Ok(res) if res.status().is_success() => res,
            _ => return false,
        };
        let descriptor: Value = match res.json().await {
            Ok(v) => v,
            Err(_) => return false,
        };
        descriptor["environmentId"].as_str() == Some(&self.record().environment_id)
    }

    /// Has T3 on the running machine publish itself over Tailscale Serve and records where:
    /// after a join and on every wake, so it holds across sleeps, restores and re-joins. Restarts
    /// T3 the first time, but never while agents work (then a later check does). With HTTPS
    /// certificates off on the tailnet nothing changes there; `tailnet_https_off` says so.
    pub async fn ensure_tailnet_serve(&self) -> Result<()> {
        if !self.record().tailscale {
            return Ok(());
        }
        let Ok(_serving) = self.serving_tailnet.try_lock() else {
            return Ok(());
        };
        if self.power() != Power::Running {
            return Ok(());
        }
        let may_restart = self.watchdog_get("/status?fresh").await.is_ok_and(|s| !s.busy);
        let out = self.shared.boat.exec_ok(&self.sandbox_id(), &tailscale::serve_script(may_restart), 240).await?;
        let report = ServeReport::parse(&out)?;
        let suffix = tailscale::local_suffix().await;
        let (url, https_off) = serve_outcome(&report, suffix.as_deref());
        tracing::info!("Tailscale Serve on {}: {:?}", self.id(), report.state);
        if let Some(url) = url
            && url != self.record().tailnet_url
        {
            self.update_record(|r| r.tailnet_url = url);
            self.reset_tailnet_check();
        }
        let mut off = self.tailnet_https_off.lock().unwrap();
        if *off != https_off {
            *off = https_off;
            self.shared.changed();
        }
        Ok(())
    }

    /// A one-time link that pairs T3 Code on another device of the tailnet with the machine,
    /// through its Tailscale Serve address. Wakes it: its own server mints the link.
    pub async fn tailnet_pairing_link(&self) -> Result<String> {
        let record = self.record();
        if !record.tailscale {
            bail!("{} isn't on your tailnet", record.name);
        }
        self.ensure_awake().await?;
        if self.record().tailnet_url.is_none() {
            self.kept_awake(self.ensure_tailnet_serve()).await?;
        }
        let Some(origin) = self.record().tailnet_url else {
            return Err(if self.tailnet_https_off() {
                anyhow!("HTTPS certificates are off on your tailnet; turn them on at {}", tailscale::HTTPS_SETTINGS_URL)
            } else {
                anyhow!(
                    "{} isn't published on your tailnet yet: this computer must be on the same tailnet, and the first time its agents must be idle",
                    record.name
                )
            });
        };
        let credential = self.pairing_credential("t3boat: tailnet pairing", None).await?;
        tailscale::pairing_url(&origin, &credential)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tailnet_route_needs_a_running_machine_and_a_recent_answer() {
        let now = Instant::now();
        let ago = |secs| now.checked_sub(Duration::from_secs(secs)).unwrap();
        let good = RouteCheck { ok: true, at: Some(ago(5)) };
        assert!(good.usable(true, true, now));
        // Asleep, waking or stopping: Boat, which wakes it.
        assert!(!good.usable(false, true, now));
        // Not published on the tailnet.
        assert!(!good.usable(true, false, now));
        // Never checked, failed, or an answer from too long ago.
        assert!(!RouteCheck::default().usable(true, true, now));
        assert!(!RouteCheck { ok: false, at: Some(ago(5)) }.usable(true, true, now));
        assert!(!RouteCheck { ok: true, at: Some(ago(80)) }.usable(true, true, now));
    }

    #[test]
    fn the_tailnet_route_is_checked_when_new_failed_or_old() {
        let now = Instant::now();
        let ago = |secs| now.checked_sub(Duration::from_secs(secs)).unwrap();
        // Came up, or failed just now (`tailnet_failed` clears the time).
        assert!(RouteCheck::default().due(true, true, now));
        assert!(RouteCheck { ok: false, at: None }.due(true, true, now));
        assert!(!RouteCheck { ok: true, at: Some(ago(10)) }.due(true, true, now));
        assert!(!RouteCheck { ok: false, at: Some(ago(10)) }.due(true, true, now));
        assert!(RouteCheck { ok: true, at: Some(ago(30)) }.due(true, true, now));
        assert!(RouteCheck { ok: false, at: Some(ago(45)) }.due(true, true, now));
        // Nothing to check on a sleeping or unpublished machine.
        assert!(!RouteCheck::default().due(false, true, now));
        assert!(!RouteCheck::default().due(true, false, now));
    }

    #[test]
    fn a_serve_report_keeps_only_a_name_of_this_computers_tailnet() {
        let report = |state, dns: &str| ServeReport { state, dns_name: dns.into() };
        let suffix = Some("tail1234.ts.net");
        let on = report(ServeState::On, "t3boat-dev.tail1234.ts.net");
        assert_eq!(serve_outcome(&on, suffix), (Some(Some("https://t3boat-dev.tail1234.ts.net".into())), false));
        let starting = report(ServeState::Starting, "t3boat-dev.tail1234.ts.net");
        assert_eq!(serve_outcome(&starting, suffix).0, Some(Some("https://t3boat-dev.tail1234.ts.net".into())));
        // Another tailnet, or a name that is no tailnet node: cleared, so no token goes there.
        let foreign = report(ServeState::On, "t3boat-dev.evil.example");
        assert_eq!(serve_outcome(&foreign, suffix), (Some(None), false));
        // This computer off the tailnet: nothing to check against, the address stays as it was.
        assert_eq!(serve_outcome(&on, None), (None, false));
        // HTTPS certificates off: no address, and a note.
        assert_eq!(serve_outcome(&report(ServeState::HttpsOff, "t3boat-dev.tail1234.ts.net"), suffix), (Some(None), true));
        assert_eq!(serve_outcome(&report(ServeState::OffTailnet, ""), suffix), (Some(None), false));
        // Waiting for agents to finish: unchanged.
        assert_eq!(serve_outcome(&report(ServeState::Busy, "t3boat-dev.tail1234.ts.net"), suffix), (None, false));
    }
}

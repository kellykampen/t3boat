import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { useMachineDetails, useLatestThread } from "../api";
import type { MachineDetails as Details, LocalProject, MachineView, Size, TailscaleMode } from "../types";
import {
  ACTIONS,
  autoSleep,
  canJoinTailnet,
  duration,
  hourly,
  STAY_AWAKE,
  keepAwake,
  machineKey,
  money,
  powerAction,
  openIn,
  primaryAction,
  resizeHint,
  resizeTo,
  seedOf,
  shortDate,
  signInTo,
  tailnetHost,
  tailnetSsh,
  sizeOf,
  timeLeft,
  type Action,
} from "../machine";
import { AgentName } from "./AgentLogos";
import { BoatIcon } from "./BoatIcon";
import { DeleteErrorNote, Dots, useArmed, useDetail, useEscape, useSecsLeft } from "./common";
import { Icon } from "./Icons";
import { ProjectPicker } from "./ProjectPicker";
import { SubHeader } from "./SubHeader";

interface Props {
  machine: MachineView;
  sizes: Size[];
  /** latest UiState; every new one triggers a details refetch */
  version: unknown;
  pending: string | null;
  deleteError: string | null;
  /** opened from the row's "Add a project…" or "Add one": start with the project picker open */
  pickProject: boolean;
  /** Settings › Tailscale */
  tailscaleMode: TailscaleMode;
  onAction: (a: Action) => void;
  onAddProject: (p: LocalProject) => void;
  onDismissDeleteError: () => void;
  onError: (text: string) => void;
  onOpenSettings: () => void;
  onBack: () => void;
}

export function MachineDetails({
  machine: m,
  sizes,
  version,
  pending,
  deleteError,
  pickProject,
  tailscaleMode,
  onAction,
  onAddProject,
  onDismissDeleteError,
  onError,
  onOpenSettings,
  onBack,
}: Props) {
  const d = useMachineDetails(m.id, version, onError);
  const busy = !!pending;
  const gone = m.state === "missing";
  const latestThread = useLatestThread(m.id);
  const primary = primaryAction(m) ?? (m.paired ? openIn(latestThread) : ACTIONS.pair);
  const [primaryArmed, armPrimary] = useArmed();

  const runPrimary = () =>
    primary.cmd === "delete_machine" ? armPrimary(() => onAction(primary)) : onAction(primary);

  return (
    <div className={`view details state-${m.state}`}>
      <SubHeader title={m.name} onBack={onBack} />
      <div className="view-body details-body">
        <Glance m={m} size={sizeOf(sizes, m.size)} d={d} />
        {gone ? null : <Power m={m} busy={busy} onAction={onAction} />}
        <Agents m={m} d={d} pending={pending} onAction={onAction} />
        <Connect
          m={m}
          d={d}
          pending={pending}
          tailscaleMode={tailscaleMode}
          onAction={onAction}
          onOpenSettings={onOpenSettings}
        />
        <Projects m={m} pickProject={pickProject} onAddProject={onAddProject} />
        <MachineMore
          m={m}
          sizes={sizes}
          busy={busy}
          deleteError={deleteError}
          onAction={onAction}
          onDismissDeleteError={onDismissDeleteError}
        />
        <p className="details-meta">
          {d ? (
            <>
              <span>T3 {d.t3Version}</span>
              <span className="sep">·</span>
              <span>created {shortDate(d.createdAt)}</span>
              <span className="sep">·</span>
              <span className="selectable details-id">{m.id}</span>
            </>
          ) : (
            <span className="skel skel-meta" />
          )}
        </p>
      </div>
      <div className="view-foot details-foot">
        <button
          className={`btn btn-lg ${primaryArmed ? "btn-danger" : primary.solid ? "btn-solid" : "btn-outline"}`}
          disabled={busy}
          aria-busy={busy}
          onClick={runPrimary}
        >
          {pending ? (
            <>
              <Dots /> {pending}
            </>
          ) : primaryArmed ? (
            "Confirm"
          ) : (
            <>
              {primary.label}
              {primary.cmd === "open_machine" && !primary.solid ? <Icon name="open" size={10} /> : null}
            </>
          )}
        </button>
      </div>
    </div>
  );
}

/** What a user scans for first: is it working, what is it, what does it cost. */
function Glance({ m, size, d }: { m: MachineView; size: Size | undefined; d: Details | null }) {
  // "Working · 2 threads · 3 subagents" → headline "Working", the rest underneath
  const detail = useDetail(m);
  const [headline, ...rest] = detail.split(" · ");
  return (
    <section className="glance" aria-label={`${m.name}: ${detail}`}>
      <div className="glance-top">
        <div className="glance-boat">
          <BoatIcon state={m.state} seed={seedOf(machineKey(m))} />
        </div>
        <div className="glance-text">
          <span className="glance-state">
            <i className="state-dot" aria-hidden />
            {headline}
          </span>
          {rest.length ? <span className="glance-detail">{rest.join(" · ")}</span> : null}
          <span className="glance-spec">
            <span className="chip">{m.size}</span>
            {size ? (
              <>
                <span>{size.spec}</span>
                <span className="glance-rate">{hourly(size.hourly)}</span>
              </>
            ) : null}
          </span>
        </div>
      </div>
      <div className="glance-costs">
        <Stat label="today" value={d && money(d.costToday)} />
        <Stat label="this month" value={d && money(d.costMonth)} />
        <Stat label="awake today" value={d && duration(d.awakeSecsToday)} />
      </div>
    </section>
  );
}

function Stat({ label, value }: { label: string; value: string | null }) {
  return (
    <div className="cost">
      <span className="caption">{label}</span>
      <span className="cost-value">{value ?? <span className="skel skel-value" />}</span>
    </div>
  );
}

function Section({ title, count, extra, children }: { title: string; count?: number; extra?: ReactNode; children: ReactNode }) {
  return (
    <section className="section">
      <div className="section-head">
        <span className="caption">
          {title}
          {count ? <span className="count">{count}</span> : null}
        </span>
        <span className="section-rule" />
        {extra}
      </div>
      {children}
    </section>
  );
}

function Power({ m, busy, onAction }: { m: MachineView; busy: boolean; onAction: (a: Action) => void }) {
  const power = powerAction(m);
  // optimistic, so the switch moves on click; the next state is the source of truth
  const [want, setWant] = useState<boolean | null>(null);
  useEffect(() => {
    if (!busy) setWant(null);
  }, [busy]);
  const on = want ?? !m.keepAwakeForever;

  return (
    <Section title="power">
      <label className="toggle">
        <span className="toggle-text">
          <span className="toggle-name">Auto sleep</span>
          <span className="hint">
            {on ? "Sleeps 5 min after agents finish; asleep is free" : "Stays up and bills until you turn this back on"}
          </span>
        </span>
        <input
          type="checkbox"
          role="switch"
          className="switch"
          checked={on}
          disabled={busy}
          onChange={(e) => {
            setWant(e.target.checked);
            onAction(autoSleep(e.target.checked));
          }}
        />
      </label>
      <div className="power-row">
        <button className="btn btn-sm btn-quiet" disabled={busy || !power} onClick={() => power && onAction(power)}>
          {(power ?? ACTIONS.sleep).label}
        </button>
        {on ? <StayAwake m={m} busy={busy} onAction={onAction} /> : null}
      </div>
    </Section>
  );
}

/** Like caffeinate: a one-time "stay up at least this long", which simply runs out. */
function StayAwake({ m, busy, onAction }: { m: MachineView; busy: boolean; onAction: (a: Action) => void }) {
  const [open, setOpen] = useState(false);
  const close = useCallback(() => setOpen(false), []);
  useEscape(open, close);
  const left = useSecsLeft(m.keepAwakeUntil);

  if (m.keepAwakeUntil !== null) {
    return (
      <div className="stay-awake is-on" role="status">
        <i className="state-dot" aria-hidden />
        <span className="stay-awake-left">Awake · {timeLeft(left)} left</span>
        <button className="btn btn-link" disabled={busy} onClick={() => onAction(keepAwake(0))}>
          Stop
        </button>
      </div>
    );
  }
  return (
    <div className="stay-awake">
      <button
        className={`btn btn-sm btn-quiet stay-awake-btn${open ? " is-open" : ""}`}
        aria-expanded={open}
        disabled={busy}
        onClick={() => setOpen((o) => !o)}
      >
        Stay awake for…
        <Icon name="chevron" size={8} />
      </button>
      {open ? (
        <div className="segmented stay-awake-options" role="menu">
          {STAY_AWAKE.map((min) => (
            <button
              key={min}
              role="menuitem"
              onClick={() => {
                setOpen(false);
                onAction(keepAwake(min));
              }}
            >
              {duration(min * 60)}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

function Agents({
  m,
  d,
  pending,
  onAction,
}: {
  m: MachineView;
  d: Details | null;
  pending: string | null;
  onAction: (a: Action) => void;
}) {
  const [signingIn, setSigningIn] = useState<string | null>(null);
  return (
    <Section title="agents">
      <ul className="lines">
        {d ? (
          d.agents.map((a) => {
            const running = signingIn === a.name && pending === signInTo(a.name).busy;
            return (
              <li key={a.name} className="line">
                <span className="line-main">
                  <AgentName name={a.name} />
                </span>
                {a.signedIn ? (
                  <span className="line-ok">
                    <Icon name="check" size={10} /> signed in
                  </span>
                ) : m.signedOut.includes(a.name) ? (
                  <button
                    className="btn btn-sm btn-outline"
                    disabled={!!pending || m.state === "missing"}
                    aria-busy={running}
                    onClick={() => {
                      setSigningIn(a.name);
                      onAction(signInTo(a.name));
                    }}
                  >
                    {running ? (
                      <>
                        <Dots /> Signing in…
                      </>
                    ) : (
                      "Sign in"
                    )}
                  </button>
                ) : (
                  <span className="caption">signed out</span>
                )}
              </li>
            );
          })
        ) : (
          <Skeleton rows={2} />
        )}
      </ul>
    </Section>
  );
}

/** T3 Connect: the user's phone or a browser reach the machine with their T3 account. Tailscale: their tailnet. */
function Connect({
  m,
  d,
  pending,
  tailscaleMode,
  onAction,
  onOpenSettings,
}: {
  m: MachineView;
  d: Details | null;
  pending: string | null;
  tailscaleMode: TailscaleMode;
  onAction: (a: Action) => void;
  onOpenSettings: () => void;
}) {
  const running = pending === ACTIONS.connect.busy;
  return (
    <Section title="other devices">
      <ul className="lines">
        {d ? (
          <>
            <li className="line">
              <span className="line-main">T3 Connect</span>
              {d.connect ? (
                <span className="line-ok">
                  <Icon name="check" size={10} /> on
                </span>
              ) : (
                <button
                  className="btn btn-sm btn-outline"
                  disabled={!!pending || m.state === "missing"}
                  aria-busy={running}
                  onClick={() => onAction(ACTIONS.connect)}
                >
                  {running ? (
                    <>
                      <Dots /> {ACTIONS.connect.busy}
                    </>
                  ) : (
                    ACTIONS.connect.label
                  )}
                </button>
              )}
            </li>
            {d.connect ? null : (
              <li className="line">
                <span className="line-main line-dim">T3 Code on your phone or in a browser, with your T3 account</span>
              </li>
            )}
          </>
        ) : (
          <Skeleton rows={1} />
        )}
        <Tailscale m={m} mode={tailscaleMode} pending={pending} onAction={onAction} onOpenSettings={onOpenSettings} />
      </ul>
    </Section>
  );
}

/** Whether it's on the user's tailnet, and joining it with a browser sign-in (any machine, old ones too). */
function Tailscale({
  m,
  mode,
  pending,
  onAction,
  onOpenSettings,
}: {
  m: MachineView;
  mode: TailscaleMode;
  pending: string | null;
  onAction: (a: Action) => void;
  onOpenSettings: () => void;
}) {
  const [copied, setCopied] = useState<"ok" | "failed" | null>(null);
  useEffect(() => {
    if (!copied) return;
    const t = setTimeout(() => setCopied(null), 1500);
    return () => clearTimeout(t);
  }, [copied]);
  const ssh = tailnetSsh(m);
  const copy = () =>
    navigator.clipboard.writeText(ssh).then(
      () => setCopied("ok"),
      () => setCopied("failed"),
    );

  if (m.tailscale) {
    const pairing = pending === ACTIONS.tailnetPair.busy;
    return (
      <>
        <li className="line line-tailscale">
          <span>Tailscale</span>
          <span className="line-main line-host selectable" title={`On your tailnet as ${tailnetHost(m)}`}>
            {tailnetHost(m)}
          </span>
          <button className="btn btn-sm btn-quiet copy-btn" title={`Copy “${ssh}”`} onClick={copy}>
            {copied === "ok" ? (
              <>
                <Icon name="check" size={10} /> Copied
              </>
            ) : copied === "failed" ? (
              "Couldn't copy"
            ) : (
              <>
                <Icon name="copy" size={10} /> Copy ssh
              </>
            )}
          </button>
        </li>
        {m.tailnetUrl ? (
          <>
            <li className="line line-tailnet-url">
              <span
                className="line-main line-host selectable"
                title={`T3 over Tailscale Serve${m.tailnetRoute ? "; t3boat reaches it over the tailnet now" : ""}`}
              >
                https://
                <wbr />
                <span className="host-name">{m.tailnetUrl.replace(/^https:\/\//, "")}</span>
              </span>
              {m.tailnetRoute ? <span className="chip chip-tailnet chip-route">via tailnet</span> : null}
            </li>
            <li className="line">
              <span className="line-main line-dim">T3 Code on another tailnet device</span>
              <button
                className="btn btn-sm btn-quiet copy-btn"
                title="Copy a one-time link that pairs T3 Code on another device of your tailnet"
                disabled={!!pending || m.state === "missing"}
                aria-busy={pairing}
                onClick={() => onAction(ACTIONS.tailnetPair)}
              >
                {pairing ? (
                  <>
                    <Dots /> Copying…
                  </>
                ) : (
                  <>
                    <Icon name="copy" size={10} /> Pairing link
                  </>
                )}
              </button>
            </li>
          </>
        ) : m.tailnetHttpsOff ? (
          <li className="line">
            <span className="line-main line-dim line-wrap">
              HTTPS certificates are off on your tailnet, so T3 is reached over Boat. Turn them on in Tailscale's DNS
              settings.
            </span>
            <button className="btn btn-sm btn-outline" onClick={() => onAction(ACTIONS.tailnetHttps)}>
              {ACTIONS.tailnetHttps.label}
            </button>
          </li>
        ) : null}
      </>
    );
  }
  const join = m.tailscaleJoin;
  if (join?.state === "starting" || join?.state === "waitingForSignIn") {
    const waiting = join.state === "waitingForSignIn";
    return (
      <>
        <li className="line">
          <span className="line-main">Tailscale</span>
          <span className="line-status caption">
            <Dots /> {!waiting ? "Starting…" : join.url ? "Waiting for sign-in" : "Waiting for approval"}
          </span>
          {waiting && join.url ? (
            <button className="btn btn-sm btn-outline" onClick={() => onAction(ACTIONS.tailnetLink)}>
              Open link
            </button>
          ) : null}
          <button className="btn btn-link" onClick={() => onAction(ACTIONS.tailnetCancel)}>
            Cancel
          </button>
        </li>
        <li className="line">
          <span className="line-main line-dim line-wrap">
            {!waiting
              ? "Installing Tailscale; the sign-in page opens next"
              : join.url
                ? "Sign in to Tailscale in the browser and click Connect"
                : "A tailnet admin has to approve the device in Tailscale's admin console"}
          </span>
        </li>
      </>
    );
  }
  if (mode !== "off") {
    const connect = canJoinTailnet(m, mode);
    return (
      <>
        <li className="line">
          <span className="line-main">Tailscale</span>
          <button className="btn btn-sm btn-outline" disabled={!connect} onClick={() => onAction(ACTIONS.tailnet)}>
            {join?.state === "error" ? "Try again" : "Connect"}
          </button>
        </li>
        <li className="line">
          <span className={`line-main line-wrap ${join?.state === "error" ? "line-error" : "line-dim"}`}>
            {join?.state === "error"
              ? `Couldn't join: ${join.message}`
              : `Sign in with your browser; it joins as ${tailnetHost(m)} with Tailscale SSH`}
          </span>
        </li>
      </>
    );
  }
  return (
    <li className="line">
      <span className="line-main line-dim">Tailscale: off</span>
      <button className="btn btn-link" onClick={onOpenSettings}>
        Set up in Settings
      </button>
    </li>
  );
}

/** Its T3 projects, and adding one the way T3 Code's "Add to another machine" does. */
function Projects({
  m,
  pickProject,
  onAddProject,
}: {
  m: MachineView;
  pickProject: boolean;
  onAddProject: (p: LocalProject) => void;
}) {
  const gone = m.state === "missing";
  if (gone && !m.projects.length) return null;
  return (
    <Section title="projects" count={m.projects.length}>
      <ul className="lines">
        {m.projects.map((p) => (
          <li key={p.id} className="line">
            <span className="line-main" title={p.title}>
              {p.title}
            </span>
            {p.status ? (
              <span className="line-status caption" title={p.status}>
                <Dots /> {p.status}
              </span>
            ) : null}
          </li>
        ))}
        {!m.projects.length && !gone ? (
          <li className="line">
            <span className="line-main line-dim">Cloned from its git remote, .env files included</span>
          </li>
        ) : null}
        {gone ? null : (
          <li className="line line-send">
            <ProjectPicker m={m} initialOpen={pickProject} onAdd={onAddProject} />
          </li>
        )}
      </ul>
    </Section>
  );
}

/** The rare stuff: size and delete, folded away. */
function MachineMore({
  m,
  sizes,
  busy,
  deleteError,
  onAction,
  onDismissDeleteError,
}: {
  m: MachineView;
  sizes: Size[];
  busy: boolean;
  deleteError: string | null;
  onAction: (a: Action) => void;
  onDismissDeleteError: () => void;
}) {
  const ref = useRef<HTMLDetailsElement>(null);
  const [deleteArmed, armDelete] = useArmed();
  const size = sizeOf(sizes, m.size);
  const gone = m.state === "missing";

  // a refused delete (from here or the list) must be visible
  useEffect(() => {
    if (deleteError && ref.current) ref.current.open = true;
  }, [deleteError]);

  return (
    <details ref={ref} className="section more">
      <summary className="section-head">
        <span className="caption">machine</span>
        <span className="section-rule" />
        <span className="caption more-peek">
          {gone ? "gone" : size ? `${m.size} · ${hourly(size.hourly)}` : m.size}
        </span>
        <Icon name="chevron" size={8} />
      </summary>
      <div className="more-body">
        {gone ? null : (
          <div className="more-group">
            <span className="toggle-name">Size</span>
            <div className="segmented" role="radiogroup" aria-label="Size">
              {sizes.map((s) => (
                <button
                  key={s.id}
                  role="radio"
                  aria-checked={s.id === m.size}
                  className={s.id === m.size ? "is-on" : undefined}
                  disabled={busy}
                  onClick={() => s.id !== m.size && onAction(resizeTo(s.id))}
                >
                  {s.id}
                  <small>{s.spec}</small>
                  <small>{hourly(s.hourly)}</small>
                </button>
              ))}
            </div>
            <span className="hint">{resizeHint(m)}</span>
          </div>
        )}
        <div className="more-group">
          <button
            className={`btn btn-sm ${deleteArmed ? "btn-danger" : "btn-quiet is-danger"} delete-btn`}
            disabled={busy}
            onClick={() => armDelete(() => onAction(ACTIONS.del))}
          >
            {deleteArmed ? "Click again to delete forever" : "Delete machine…"}
          </button>
          {deleteError ? (
            <DeleteErrorNote
              error={deleteError}
              busy={busy}
              onDeleteAnyway={() => onAction(ACTIONS.deleteAnyway)}
              onKeep={onDismissDeleteError}
            />
          ) : null}
        </div>
      </div>
    </details>
  );
}

function Skeleton({ rows }: { rows: number }) {
  return (
    <>
      {Array.from({ length: rows }, (_, i) => (
        <li key={i} className="line" aria-hidden>
          <span className="skel" style={{ width: `${40 - i * 10}%` }} />
        </li>
      ))}
    </>
  );
}

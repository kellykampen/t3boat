import { type FormEvent, useEffect, useState } from "react";
import type { McpClient, Settings, TailscaleMode, UiState } from "../types";
import { AgentName, AgentNames } from "./AgentLogos";
import { Icon } from "./Icons";
import { Dots } from "./common";
import { SubHeader } from "./SubHeader";

const CLIENTS: { id: McpClient; name: string }[] = [
  { id: "claudeCode", name: "Claude Code" },
  { id: "codex", name: "Codex" },
];

const MODES: { id: TailscaleMode; label: string; note: string }[] = [
  { id: "off", label: "Off", note: "not joined" },
  { id: "browser", label: "Browser", note: "recommended" },
  { id: "key", label: "Auth key", note: "advanced" },
];

interface Props {
  state: UiState;
  onSave: (s: Settings) => void;
  /** never rejects; errors are shown by the caller */
  onAddMcp: (client: McpClient) => Promise<void>;
  /** never rejects; false = it failed, errors are shown by the caller */
  onSaveTailscaleKey: (key: string) => Promise<boolean>;
  /** never rejects; errors are shown by the caller */
  onRemoveTailscaleKey: () => Promise<void>;
  onQuit: () => void;
  onBack: () => void;
}

export function SettingsView({
  state,
  onSave,
  onAddMcp,
  onSaveTailscaleKey,
  onRemoveTailscaleKey,
  onQuit,
  onBack,
}: Props) {
  // optimistic copy; the next "state" event is the source of truth
  const [s, setS] = useState(state.settings);
  useEffect(() => setS(state.settings), [state.settings]);

  // hidden by default so screen recordings don't leak the email
  const [showEmail, setShowEmail] = useState(false);
  const email = state.account?.identifier;
  const plan = state.account?.plan;

  const [adding, setAdding] = useState<McpClient | null>(null);
  const add = (client: McpClient) => {
    setAdding(client);
    onAddMcp(client).finally(() => setAdding(null));
  };
  const missing = CLIENTS.filter((c) => state.mcp[c.id] === null).map((c) => c.name);

  // the key lives here only until saved; the backend never sends it back
  const [tsKey, setTsKey] = useState("");
  const [tsBusy, setTsBusy] = useState(false);
  // "Replace": the form again while a key stays saved until the new one is
  const [replacing, setReplacing] = useState(false);
  const saveKey = (e: FormEvent) => {
    e.preventDefault();
    if (!tsKey.trim() || tsBusy) return;
    setTsBusy(true);
    onSaveTailscaleKey(tsKey)
      .then((ok) => {
        if (!ok) return;
        setTsKey("");
        setReplacing(false);
      })
      .finally(() => setTsBusy(false));
  };
  const cancelReplace = () => {
    setTsKey("");
    setReplacing(false);
  };
  const onTailnet = state.machines.filter((m) => m.tailscale).length;
  const removeKey = () => {
    setTsBusy(true);
    onRemoveTailscaleKey().finally(() => setTsBusy(false));
  };

  const change = (p: Partial<Settings>) => {
    const next = { ...s, ...p };
    setS(next);
    onSave(next);
  };

  return (
    <div className="view">
      <SubHeader title="Settings" onBack={onBack} />
      <div className="view-body settings">
        <p className="hint">
          Machines sleep 5 minutes after their agents finish and wake by themselves when you use them in T3 Code.
        </p>
        <label className="setting setting-row">
          <span className="setting-name">Start at login</span>
          <input
            type="checkbox"
            role="switch"
            className="switch"
            checked={s.startAtLogin}
            onChange={(e) => change({ startAtLogin: e.target.checked })}
          />
        </label>
        <div className="setting">
          <span className="caption">mcp</span>
          <span className="setting-name">
            Let <AgentNames names={CLIENTS.map((c) => c.name)} /> control t3boat
          </span>
          <div className="mcp-buttons">
            {CLIENTS.map((c) => {
              const added = state.mcp[c.id];
              return added ? (
                <span key={c.id} className="mcp-added">
                  <Icon name="check" size={10} />
                  <span>
                    Added to <AgentName name={c.name} />
                  </span>
                </span>
              ) : (
                <button
                  key={c.id}
                  className="btn btn-sm btn-outline"
                  disabled={added === null || adding !== null}
                  aria-busy={adding === c.id}
                  onClick={() => add(c.id)}
                >
                  {adding === c.id ? (
                    <>
                      <Dots /> Adding…
                    </>
                  ) : (
                    <span>
                      Add to <AgentName name={c.name} />
                    </span>
                  )}
                </button>
              );
            })}
          </div>
          <span className="hint">
            {missing.length ? (
              <>
                <AgentNames names={missing} /> {missing.length === 1 ? "isn't" : "aren't"} installed on this computer
              </>
            ) : (
              "Their agents can then create, wake and send work to machines"
            )}
          </span>
        </div>
        <div className="setting">
          <span className="caption" id="tailscale-caption">
            tailscale
          </span>
          <span className="setting-name">Join machines to your tailnet</span>
          <div className="segmented tailscale-modes" role="radiogroup" aria-labelledby="tailscale-caption">
            {MODES.map((o) => (
              <button
                key={o.id}
                role="radio"
                aria-checked={s.tailscale === o.id}
                className={s.tailscale === o.id ? "is-on" : undefined}
                onClick={() => s.tailscale !== o.id && change({ tailscale: o.id })}
              >
                {o.label}
                <small>{o.note}</small>
              </button>
            ))}
          </div>
          {s.tailscale === "key" ? (
            state.tailscale.configured && !replacing ? (
              <div className="tailscale-row is-saved">
                <span className="mcp-added">
                  <Icon name="check" size={10} />
                  <span>Key saved · new machines join with it</span>
                </span>
                <button className="btn btn-sm btn-quiet" disabled={tsBusy} onClick={() => setReplacing(true)}>
                  Replace
                </button>
                <button className="btn btn-sm btn-quiet" disabled={tsBusy} aria-busy={tsBusy} onClick={removeKey}>
                  Remove
                </button>
              </div>
            ) : (
              <form className={`tailscale-row${replacing ? " is-replacing" : ""}`} onSubmit={saveKey}>
                <span className="prompt-input tailscale-input">
                  <input
                    id="tailscale-key"
                    aria-label="Tailscale auth key"
                    autoFocus={replacing}
                    type="password"
                    placeholder="tskey-auth-…"
                    aria-describedby="tailscale-hint"
                    value={tsKey}
                    onChange={(e) => setTsKey(e.target.value)}
                    spellCheck={false}
                    autoComplete="off"
                  />
                </span>
                <button
                  type="submit"
                  className="btn btn-sm btn-outline"
                  disabled={!tsKey.trim() || tsBusy}
                  aria-busy={tsBusy}
                >
                  {tsBusy ? (
                    <>
                      <Dots /> Saving…
                    </>
                  ) : (
                    "Save"
                  )}
                </button>
                {replacing ? (
                  <button type="button" className="btn btn-sm btn-quiet" disabled={tsBusy} onClick={cancelReplace}>
                    Cancel
                  </button>
                ) : null}
              </form>
            )
          ) : null}
          <span className="hint" id="tailscale-hint">
            {s.tailscale === "browser"
              ? "Sign in with your browser: each new machine opens a Tailscale sign-in page; click Connect. It joins as you, so its key expires after 180 days unless you disable key expiry for it in the admin console."
              : s.tailscale === "key"
                ? "New machines join with a saved auth key, no sign-in. Use a reusable, pre-authorized key tagged e.g. tag:t3boat."
                : "New machines stay off your tailnet."}
            {s.tailscale !== "off" ? " They show up as t3boat-‹name› with Tailscale SSH; asleep, as offline." : ""}
            {onTailnet ? ` ${onTailnet} ${onTailnet === 1 ? "machine is" : "machines are"} on your tailnet.` : ""}
          </span>
        </div>
      </div>
      <div className="view-foot settings-foot">
        <span className="setting-value" title="Boat account">
          {email && (
            <button className="account-email" onClick={() => setShowEmail((v) => !v)}>
              {showEmail ? email : "Show email"}
            </button>
          )}
          {email && plan && " · "}
          {plan}
        </span>
        <button className="btn btn-sm btn-quiet" onClick={onQuit}>
          Quit t3boat
        </button>
      </div>
    </div>
  );
}

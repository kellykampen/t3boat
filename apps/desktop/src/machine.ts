import type { MachineState, MachineView, Size, TailscaleMode } from "./types";

const TAGS: Record<MachineState, string> = {
  working: "working",
  idle: "idle",
  sleeping: "dozing off",
  asleep: "zzz",
  waking: "waking",
  creating: "building",
  error: "error",
  missing: "gone",
  unknown: "…",
};

export const stateTag = (s: MachineState) => TAGS[s];

/** Stable key: creating rows have no id yet. */
export const machineKey = (m: MachineView) => m.id || `new:${m.name}`;

/** Its name on the user's tailnet (provision.sh joins it with `--hostname=t3boat-<name>`). */
export const tailnetHost = (m: MachineView) => `t3boat-${m.name}`;
/** Tailscale SSH maps to the machine's `user` account (README → Tailscale). */
export const tailnetSsh = (m: MachineView) => `ssh user@${tailnetHost(m)}`;

export function seedOf(key: string): number {
  let h = 0;
  for (let i = 0; i < key.length; i++) h = (h * 31 + key.charCodeAt(i)) | 0;
  return (h >>> 0) / 4294967296;
}

/** A backend command for one machine, with the arguments it needs beyond the id. */
export type Action = {
  label: string;
  busy: string;
  /** solid button when primary; outline otherwise */
  solid?: boolean;
} & (
  | { cmd: "open_machine"; thread?: string }
  | { cmd: "pair_machine" | "wake_machine" | "sleep_machine" | "turn_on_connect" }
  /** the tailnet's browser sign-in: never a tracked action, its progress is `tailscaleJoin` */
  | { cmd: "tailscale_connect" | "tailscale_open_sign_in" | "tailscale_cancel" }
  | { cmd: "resize_machine"; size: string }
  /** null = until stopped (auto sleep off), 0 ends it */
  | { cmd: "keep_awake"; minutes: number | null }
  /** the agent's name as `signedOut` lists it */
  | { cmd: "sign_in_agent"; agent: string }
  /** discard: delete even while work runs on it */
  | { cmd: "delete_machine"; discard: boolean }
);

/** UI-only ⋯ menu entry: opens the project picker in the details view. */
export const PICK_PROJECT = { kind: "pickProject", label: "Add a project…" } as const;

export type MenuItem = Action | typeof PICK_PROJECT;

export const signInTo = (agent: string): Action => ({
  cmd: "sign_in_agent",
  label: `Sign in ${agent}`,
  busy: "Signing in…",
  agent,
});

export const ACTIONS = {
  pair: { cmd: "open_machine", label: "Add to T3 Code", busy: "Sending…", solid: true },
  repair: { cmd: "pair_machine", label: "Add to T3 Code again", busy: "Sending…" },
  wake: { cmd: "wake_machine", label: "Wake now", busy: "Waking…" },
  retry: { cmd: "wake_machine", label: "Try waking", busy: "Waking…" },
  sleep: { cmd: "sleep_machine", label: "Sleep now", busy: "Sleeping…" },
  connect: { cmd: "turn_on_connect", label: "Turn on", busy: "Approve in browser…" },
  tailnet: { cmd: "tailscale_connect", label: "Connect to tailnet", busy: "Connecting…" },
  tailnetLink: { cmd: "tailscale_open_sign_in", label: "Open link", busy: "Opening…" },
  tailnetCancel: { cmd: "tailscale_cancel", label: "Cancel", busy: "Cancelling…" },
  remove: { cmd: "delete_machine", label: "Remove", busy: "Removing…", discard: true },
  del: { cmd: "delete_machine", label: "Delete", busy: "Deleting…", discard: false },
  deleteAnyway: { cmd: "delete_machine", label: "Delete anyway", busy: "Deleting…", discard: true },
} satisfies Record<string, Action>;

/** The one obvious thing to do with a machine right now, if any. */
export function primaryAction(m: MachineView): Action | null {
  switch (m.state) {
    case "working":
    case "idle":
    case "asleep": // wakes by itself when used in T3 Code; opening it lives in the details view
      return m.paired ? null : ACTIONS.pair;
    case "error":
      return ACTIONS.retry;
    case "missing":
      return ACTIONS.remove;
    default:
      return null; // transitional: the boat + detail line say what's happening
  }
}

/** Opens the machine's latest thread in T3 Code, named so the button says where it goes. */
export const openIn = (thread: string | null): Action => ({
  cmd: "open_machine",
  label: thread ? "Open last thread in T3 Code" : "Open T3 Code",
  busy: "Opening…",
  thread: thread ?? undefined,
});

/** "Stay awake for…" choices, in minutes. */
export const STAY_AWAKE = [15, 30, 60, 120] as const;

/** A one-time "stay up at least this long"; 0 stops it early. */
export const keepAwake = (minutes: number): Action => ({
  cmd: "keep_awake",
  label: minutes ? `Stay awake for ${duration(minutes * 60)}` : "Stop staying awake",
  busy: minutes ? "Waking…" : "Stopping…",
  minutes,
});

/** The auto sleep switch: off keeps the machine up until it's turned back on. */
export const autoSleep = (on: boolean): Action => ({
  cmd: "keep_awake",
  label: on ? "Turn on auto sleep" : "Turn off auto sleep",
  busy: on ? "Turning on auto sleep…" : "Turning off auto sleep…",
  minutes: on ? 0 : null,
});

export const resizeTo = (size: string): Action => ({
  cmd: "resize_machine",
  label: `Resize to ${size}`,
  busy: "Resizing…",
  size,
});

/** A tailnet sign-in is under way (an error is not: it can be retried). */
export const joiningTailnet = (m: MachineView) =>
  m.tailscaleJoin?.state === "starting" || m.tailscaleJoin?.state === "waitingForSignIn";

/** "Connect to tailnet" applies: Tailscale is set up in Settings and the machine isn't on it or joining. */
export const canJoinTailnet = (m: MachineView, mode: TailscaleMode) =>
  mode !== "off" && !!m.id && !m.tailscale && !joiningTailnet(m) && !["creating", "missing"].includes(m.state);

/** Secondary actions for the ⋯ menu (delete is always appended by the menu). */
export function menuActions(m: MachineView, sizes: Size[], tailscale: TailscaleMode): MenuItem[] {
  if (!m.id || m.state === "missing") return [];
  const out: MenuItem[] = [];
  if (m.state === "asleep") out.push(ACTIONS.wake);
  if (m.state === "working" || m.state === "idle") out.push(ACTIONS.sleep);
  out.push(PICK_PROJECT);
  if (canJoinTailnet(m, tailscale)) out.push(ACTIONS.tailnet);
  if (m.paired) out.push(ACTIONS.repair);
  sizes.filter((s) => s.id !== m.size).forEach((s) => out.push(resizeTo(s.id)));
  return out;
}

/** Wake or sleep, whichever applies now; null mid-transition. */
export function powerAction(m: MachineView): Action | null {
  if (m.state === "asleep") return ACTIONS.wake;
  if (m.state === "working" || m.state === "idle") return ACTIONS.sleep;
  return null;
}

/** When a resize takes effect, per contract. */
export function resizeHint(m: MachineView): string {
  if (m.state === "asleep") return "Applies on next wake";
  if (m.state === "working") return "Applies after it next sleeps, never mid-turn";
  if (m.state === "idle") return "Restarts onto the new size now (about a minute)";
  return "Billed only while awake";
}

/** "2h 14m", "2h", "38m", "0m" */
export function duration(secs: number): string {
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m`;
  return m % 60 ? `${Math.floor(m / 60)}h ${m % 60}m` : `${m / 60}h`;
}

/** "42s", "4m", "1h 5m"; minutes round up. Matches the backend's `human_secs`. */
export function timeLeft(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const m = Math.ceil(secs / 60);
  return m < 60 ? `${m}m` : `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** "/Users/you/Dev/app" → "~/Dev/app" */
export const homePath = (path: string) => path.replace(/^\/(?:Users|home)\/[^/]+(?=\/|$)/, "~");

/** The machine has this repository, or is getting it. */
export const hasRepo = (m: MachineView, repoKey: string) => m.projects.some((p) => p.repoKey === repoKey);

/** Up and added to T3 Code, but nothing to work on yet: the last step of setting it up. */
export const needsProject = (m: MachineView) =>
  m.paired && !m.projects.length && !["creating", "missing", "error"].includes(m.state);

export const shortDate = (unixSecs: number) =>
  new Date(unixSecs * 1000).toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });

/** "just now", "12m ago", "3h ago", "2d ago", then a date past four weeks. */
export function ago(iso: string, now = Date.now()): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const m = Math.floor((now - then) / 60_000);
  if (m < 1) return "just now";
  if (m < 60) return `${m}m ago`;
  if (m < 24 * 60) return `${Math.floor(m / 60)}h ago`;
  if (m < 28 * 24 * 60) return `${Math.floor(m / (24 * 60))}d ago`;
  return shortDate(then / 1000);
}

/** t3boat's own sleep snapshots are named `t3boat-<machine>-<sandbox id>` (`provision::snapshot_name`). */
export const isMachineSnapshot = (name: string) => name.startsWith("t3boat-");

/** The machine a snapshot was taken of, by name when t3boat knows it (or its snapshot name says it), else its sandbox id. */
export function snapshotSource(s: { name: string; sourceSandboxId: string | null }, machines: MachineView[]): string | null {
  if (!s.sourceSandboxId) return null;
  const known = machines.find((m) => m.id && m.id === s.sourceSandboxId);
  if (known) return known.name;
  const id = s.sourceSandboxId.toLowerCase().replace(/[^a-z0-9]/g, "-");
  const fromName = isMachineSnapshot(s.name) && s.name.endsWith(`-${id}`) ? s.name.slice("t3boat-".length, -id.length - 1) : "";
  return fromName || s.sourceSandboxId;
}

export const money = (n: number) => `$${n.toFixed(2)}`;
export const hourly = (n: number) => `$${n < 0.1 ? n.toFixed(3) : n.toFixed(2)}/h`;

export const sizeOf = (sizes: Size[], id: string) => sizes.find((s) => s.id === id);

const NAMES = ["skiff", "dinghy", "sloop", "ketch", "yawl", "cutter", "dory", "schooner", "clipper", "catboat", "junk", "brig"];

export function suggestName(taken: string[]): string {
  const used = new Set(taken);
  const free = NAMES.find((n) => !used.has(n));
  if (free) return free;
  for (let i = 2; ; i++) if (!used.has(`skiff-${i}`)) return `skiff-${i}`;
}

export const NAME_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
/** What a new machine can start from, as `provision::check_start_snapshot` takes it: a saved snapshot's name or a Boat share code. */
export const SNAPSHOT_RE = /^(?:[a-z0-9][a-z0-9-]{0,127}|bsh_[A-Za-z0-9]{1,128})$/;

export function cleanName(raw: string): string {
  return raw
    .toLowerCase()
    .replace(/[\s_]+/g, "-")
    .replace(/[^a-z0-9-]/g, "")
    .slice(0, 32);
}

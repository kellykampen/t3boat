import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { call, errorText } from "../api";
import { ago, isMachineSnapshot, snapshotSource } from "../machine";
import type { MachineView, SavedSnapshot } from "../types";
import { Icon } from "./Icons";

interface Snapshots {
  list: SavedSnapshot[] | null;
  error: string | null;
  retry: () => void;
}

/** The user's saved Boat snapshots, fetched once per mount. */
function useSnapshots(): Snapshots {
  const [list, setList] = useState<SavedSnapshot[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    call("list_snapshots", undefined).then(
      (s) => live && setList(s),
      (e) => live && setError(errorText(e)),
    );
    return () => {
      live = false;
    };
  }, [attempt]);
  return {
    list,
    error,
    retry: () => {
      setError(null);
      setAttempt((n) => n + 1);
    },
  };
}

/** One row of the list: "" is "None" (Boat's base image). */
interface Option {
  value: string;
  meta: string;
}

/**
 * "start from snapshot": a combobox over the user's saved snapshots. The input stays free
 * text, so a share code or an unlisted name can still be typed; picking fills it in. A failed
 * listing only shows a note: the field keeps working without it.
 */
export function SnapshotPicker({
  id,
  value,
  onChange,
  machines,
  invalid,
  describedBy,
}: {
  id: string;
  value: string;
  onChange: (value: string) => void;
  machines: MachineView[];
  invalid: boolean;
  describedBy: string;
}) {
  const listId = useId();
  const root = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const listbox = useRef<HTMLDivElement>(null);
  const snapshots = useSnapshots();
  const [open, setOpen] = useState(false);
  // Typing filters; opening on a picked value shows the whole list again.
  const [filtering, setFiltering] = useState(false);
  const [active, setActive] = useState(0);

  const describe = (s: SavedSnapshot): Option => {
    const from = snapshotSource(s, machines);
    return { value: s.name, meta: [ago(s.createdAt), from && `from ${from}`].filter(Boolean).join(" · ") };
  };
  /** What the list shows for a filter: "None" (unfiltered only), then the groups. */
  const build = (q: string) => {
    const shown = (snapshots.list ?? []).filter(
      (s) => !q || `${s.name}\n${snapshotSource(s, machines) ?? ""}`.toLowerCase().includes(q),
    );
    const groups = [
      { label: "your machines", options: shown.filter((s) => isMachineSnapshot(s.name)).map(describe) },
      { label: "saved", options: shown.filter((s) => !isMachineSnapshot(s.name)).map(describe) },
    ].filter((g) => g.options.length);
    const none: Option | null = q ? null : { value: "", meta: "Boat's base image" };
    return { shown, groups, none, options: [...(none ? [none] : []), ...groups.flatMap((g) => g.options)] };
  };
  const { shown, groups, none, options } = build(filtering ? value.trim().toLowerCase() : "");
  const current = Math.min(active, Math.max(options.length - 1, 0));
  const optionId = (i: number) => `${listId}-${i}`;

  const show = () => {
    setFiltering(false);
    // start on what's picked, so Enter keeps it
    setActive(Math.max(0, build("").options.findIndex((o) => o.value === value)));
    setOpen(true);
  };
  const choose = (o: Option) => {
    onChange(o.value);
    setOpen(false);
    setFiltering(false);
  };

  // keep the highlighted row, and the list itself, inside the scrolling view
  useEffect(() => {
    if (open) listbox.current?.querySelector(".pick-option.is-active")?.scrollIntoView({ block: "nearest" });
  }, [open, current]);
  // the whole field, hint included, again once the list has loaded and changed height
  const loaded = snapshots.list !== null || snapshots.error !== null;
  useEffect(() => {
    if (open) (root.current?.parentElement ?? root.current)?.scrollIntoView({ block: "nearest" });
  }, [open, loaded]);

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!open) return show();
      if (options.length) setActive((current + (e.key === "ArrowDown" ? 1 : options.length - 1)) % options.length);
    } else if (e.key === "Enter" && open) {
      // picks the highlighted row; with none (no match) it just closes, keeping what was typed
      e.preventDefault();
      const o = options[current];
      if (o) choose(o);
      else setOpen(false);
    } else if (e.key === "Escape" && open) {
      // closes the list only: the view's own Escape (back) skips a handled key
      e.preventDefault();
      setOpen(false);
    }
  };

  let index = 0;
  const row = (o: Option, label: string) => {
    const i = index++;
    const on = o.value === value;
    return (
      <div
        key={`${label}:${o.value}`}
        id={optionId(i)}
        role="option"
        aria-selected={i === current}
        className={`pick-option snap-option${i === current ? " is-active" : ""}`}
        onMouseDown={(e) => e.preventDefault()}
        onMouseMove={() => setActive(i)}
        onClick={() => choose(o)}
      >
        <span className="pick-text">
          <span className="pick-title">
            <span className={`pick-name${o.value ? "" : " snap-none"}`}>{o.value || "None"}</span>
            {on ? (
              <span className="line-ok">
                <Icon name="check" size={10} />
              </span>
            ) : null}
          </span>
          {o.meta ? <span className="snap-meta">{o.meta}</span> : null}
        </span>
      </div>
    );
  };

  const note = () => {
    if (snapshots.error) {
      return (
        <p className="pick-note caption is-error">
          Couldn't list your snapshots: {snapshots.error}. You can still type a name or share code.
          <button type="button" className="btn btn-link" onMouseDown={(e) => e.preventDefault()} onClick={snapshots.retry}>
            Try again
          </button>
        </p>
      );
    }
    if (!snapshots.list) {
      return [0, 1, 2].map((i) => (
        <div key={i} className="pick-option snap-option" aria-hidden>
          <span className="pick-text">
            <span className="skel" style={{ width: `${52 - i * 9}%` }} />
            <span className="skel skel-path" style={{ width: `${34 - i * 4}%` }} />
          </span>
        </div>
      ));
    }
    if (!snapshots.list.length) {
      return <p className="pick-note caption">No saved snapshots. Type a snapshot name or a bsh_ share code.</p>;
    }
    if (!shown.length) {
      return (
        <p className="pick-note caption">
          {value.trim().startsWith("bsh_")
            ? "Share codes aren't listed: this one is used as typed"
            : `No saved snapshot matches “${value.trim()}”: used as typed`}
        </p>
      );
    }
    return null;
  };

  return (
    <div
      ref={root}
      className="snap"
      onBlur={(e) => {
        if (!root.current?.contains(e.relatedTarget as Node | null)) setOpen(false);
      }}
    >
      <span className="prompt-input">
        <span className="prompt-sign" aria-hidden>
          $
        </span>
        <input
          ref={input}
          id={id}
          role="combobox"
          aria-autocomplete="list"
          aria-expanded={open}
          aria-controls={listId}
          aria-activedescendant={open && options.length ? optionId(current) : undefined}
          aria-describedby={describedBy}
          aria-invalid={invalid}
          value={value}
          onChange={(e) => {
            onChange(e.target.value.trim());
            setFiltering(true);
            setActive(0);
            setOpen(true);
          }}
          onClick={() => (open ? setOpen(false) : show())}
          onKeyDown={onKeyDown}
          placeholder="pick one, or type a name or bsh_ code"
          spellCheck={false}
          autoComplete="off"
          autoCapitalize="off"
        />
        {value ? (
          <button
            type="button"
            className="btn btn-icon snap-btn"
            aria-label="Clear snapshot: start from Boat's base image"
            onClick={() => {
              choose({ value: "", meta: "" });
              input.current?.focus();
            }}
          >
            <Icon name="cross" size={10} />
          </button>
        ) : null}
        <button
          type="button"
          tabIndex={-1}
          className={`btn btn-icon snap-btn snap-toggle${open ? " is-open" : ""}`}
          aria-label={open ? "Hide saved snapshots" : "Show saved snapshots"}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => {
            input.current?.focus();
            if (open) setOpen(false);
            else show();
          }}
        >
          <Icon name="chevron" size={10} />
        </button>
      </span>
      {open ? (
        <div className="pick snap-list">
          <div
            ref={listbox}
            id={listId}
            role="listbox"
            aria-label="Saved snapshots"
            aria-busy={!snapshots.list && !snapshots.error}
            className="pick-scroll"
          >
            {none ? row(none, "none") : null}
            {groups.map((g) => (
              <div key={g.label} role="group" aria-labelledby={`${listId}-${g.label}`}>
                <div id={`${listId}-${g.label}`} className="snap-group caption">
                  {g.label}
                </div>
                {g.options.map((o) => row(o, g.label))}
              </div>
            ))}
            {note()}
          </div>
        </div>
      ) : null}
    </div>
  );
}

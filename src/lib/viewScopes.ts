/**
 * What each view IS, and what it filters by default.
 *
 * The filter strip's job is to say, on screen, everything the list is
 * applying. That splits into two kinds of thing and the split is the whole
 * design:
 *
 * **Locked chips are the view's identity.** "Waiting" IS held-back; a chip
 * that could take that off would leave a view with no definition and a title
 * making a claim the list no longer supports. They are labels for
 * `db::tasks::view_where`, never sent to the backend — the `view` field
 * carries them — so there is exactly one implementation and this file cannot
 * drift into a second, quieter one.
 *
 * **Default chips are removable.** Today they are one: `Status is not Done`,
 * which every open view used to apply silently inside its WHERE clause. It is
 * a real `ActiveFilter` that goes over the wire, so removing it does what it
 * looks like it does.
 *
 * Which is which is decided by whether the user can be shown the alternative:
 * Done with `not Done` on it is an empty list; Today with completed work in it
 * is a legitimate thing to want to see.
 */
import type { ActiveFilter, ListView, Project } from "@/types/task";

export interface LockedChip {
  label: string;
  /** The tooltip — the predicate said in full, where the label is a summary. */
  hint?: string;
}

/** The default every open view carries: leave finished work out. */
export const NOT_DONE: ActiveFilter = { facet: "status", op: "is_not", value: "done" };

interface Scope {
  locked: LockedChip[];
  defaults: ActiveFilter[];
}

/**
 * Keyed by view, plus `node` for an area or project (whose locked chip names
 * the node itself and so is built at render).
 *
 * The four hidden/closed views carry NO default: `not Done` inside Done is a
 * guaranteed empty list, and the Trash is not somewhere you filter by status.
 */
const SCOPES: Record<ListView | "node", Scope> = {
  inbox: {
    locked: [
      {
        label: "Unfiled & undated",
        hint: "No project, no section and no due date — give it a home or a date and it leaves, and Open still lists it. Held work, in-flight work and decisions live elsewhere",
      },
    ],
    defaults: [NOT_DONE],
  },
  today: {
    locked: [
      {
        label: "Due by today, in progress, or a decision",
        hint: "Overdue included; deferred and armed tripwires excluded until their date or condition",
      },
    ],
    defaults: [NOT_DONE],
  },
  waiting: {
    locked: [
      {
        label: "Held back",
        hint: "A start date, an armed tripwire, an open blocker, or a written reason — oldest first",
      },
    ],
    defaults: [NOT_DONE],
  },
  open: {
    locked: [
      {
        label: "Actionable now",
        hint: "Not deferred, not on a tripwire, and not blocked — a future due date does not hide a task",
      },
    ],
    defaults: [NOT_DONE],
  },
  in_review: {
    locked: [{ label: "In review", hint: "Work produced and waiting on you to accept it" }],
    defaults: [],
  },
  // No locked chip: "all open work" is what the absence of a view predicate
  // means, and a chip saying so would be furniture.
  all: { locked: [], defaults: [NOT_DONE] },
  done: { locked: [{ label: "Done" }], defaults: [] },
  archive: {
    locked: [{ label: "Archived", hint: "Deliberately set aside. Never purged, always restorable" }],
    defaults: [],
  },
  trash: {
    locked: [{ label: "Trashed", hint: "Recoverable for 30 days, then purged" }],
    defaults: [],
  },
  node: { locked: [], defaults: [NOT_DONE] },
};

/** The locked chips for what the sidebar currently has selected. */
export function lockedChips(view: ListView, project: string | null): LockedChip[] {
  if (project !== null) {
    return [
      {
        label: `In ${project}`,
        hint: "An area also rolls up the projects nested inside it",
      },
    ];
  }
  return SCOPES[view]?.locked ?? [];
}

/**
 * The chips a view opens with: its own baseline, or what this node/view
 * remembers.
 *
 * A project with no saved filters of its own inherits its parent AREA's —
 * project → area may inherit (invariant 22), the same direction `primary_repo`
 * falls back in. Without it, saving "hide blocked" on CXVentures would stop
 * applying the moment you clicked into Acme underneath it, which reads as the
 * setting not having saved.
 */
export function defaultsFor(
  view: ListView,
  project: string | null,
  saved: Record<string, ActiveFilter[]>,
  projects: Project[] = []
): ActiveFilter[] {
  const clone = (fs: ActiveFilter[]) => fs.map((f) => ({ ...f }));
  if (project !== null) {
    const record = projects.find((p) => p.name.toLowerCase() === project.toLowerCase());
    if (record) {
      const own = saved[record.id];
      if (own) return clone(own);
      const parent =
        record.parent === null
          ? undefined
          : projects.find((p) => p.name.toLowerCase() === record.parent!.toLowerCase());
      if (parent && saved[parent.id]) return clone(saved[parent.id]!);
    }
    return clone(SCOPES.node.defaults);
  }
  return clone(saved[view] ?? SCOPES[view]?.defaults ?? []);
}

/** The baseline a view has when nothing is saved for it — what Reset means. */
export function baselineFor(view: ListView, project: string | null): ActiveFilter[] {
  return (project !== null ? SCOPES.node.defaults : (SCOPES[view]?.defaults ?? [])).map((f) => ({
    ...f,
  }));
}

/** The storage key a view or node saves its filters under. */
export function scopeKey(view: ListView, project: string | null, projects: Project[]): string | null {
  if (project === null) return view;
  // By ID, not by name, for `collapsedAreas`' reason: this is a note about a
  // row, not one of the text joins a rename has to carry (invariant 17).
  const record = projects.find((p) => p.name.toLowerCase() === project.toLowerCase());
  return record?.id ?? null;
}

/** Order-insensitive equality — the chips are a set, and the strip renders
 * them grouped, so two lists differing only in order are the same filter. */
export function sameFilters(a: ActiveFilter[], b: ActiveFilter[]): boolean {
  if (a.length !== b.length) return false;
  const key = (f: ActiveFilter) => `${f.facet}:${f.op}:${f.value}`;
  const left = a.map(key).sort();
  const right = b.map(key).sort();
  return left.every((k, i) => k === right[i]);
}

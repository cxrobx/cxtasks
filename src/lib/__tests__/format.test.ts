import { describe, expect, it } from "vitest";
import {
  backgroundTier,
  describeRecurrence,
  formatAge,
  formatDue,
  formatShortId,
  isOverdue,
  parseDbTimestamp,
  repoName,
  shortenPath,
  modelLabel,
  lastHandoffRun,
  otherExecutor,
  TIER_HINTS,
  TIER_LABELS,
} from "@/lib/format";
import {
  defaultDate,
  isoToLocalInput,
  localInputToIso,
} from "@/components/shared/DateField";
import type { Task, TaskRun } from "@/types/task";

const baseTask: Task = {
  id: "t",
  title: "t",
  body: null,
  status: "todo",
  blocked_reason: null,
  priority: 0,
  project: null,
  due_at: null,
  defer_until: null,
  recurrence: null,
  parent_id: null,
  sort_order: 0,
  created_at: "2026-07-29T12:00:00.000Z",
  updated_at: "2026-07-29T12:00:00.000Z",
  completed_at: null,
  repo_path: null,
  prompt: null,
  bg_allowed: false,
  bg_executor: "claude",
  bg_intent: "work",
  bg_writes_allowed: false,
  bg_shell_allowed: false,
  bg_remote_allowed: false,
  bg_mcp_servers: null,
  reminded_at: null,
  model: null,
  section_id: null,
  deleted_at: null,
  archived_at: null,
  short_id: 1,
  tripwire: null,
  tripwire_every_minutes: null,
  tripwire_on_trip: null,
  tripwire_state: null,
  tripwire_tripped_at: null,
};

describe("formatDue", () => {
  const now = new Date("2026-07-29T15:00:00");

  it("returns null with no due date", () => {
    expect(formatDue(null, now)).toBeNull();
  });

  it("labels past due dates as overdue with a coarse unit", () => {
    expect(formatDue(new Date("2026-07-29T14:30:00").toISOString(), now)).toBe(
      "Overdue 30m"
    );
    expect(formatDue(new Date("2026-07-29T12:00:00").toISOString(), now)).toBe(
      "Overdue 3h"
    );
    expect(formatDue(new Date("2026-07-27T15:00:00").toISOString(), now)).toBe(
      "Overdue 2d"
    );
  });

  it("shows a bare time for later today", () => {
    expect(formatDue(new Date("2026-07-29T17:00:00").toISOString(), now)).toMatch(
      /5:00/
    );
  });

  it("prefixes tomorrow and names near weekdays", () => {
    expect(formatDue(new Date("2026-07-30T17:00:00").toISOString(), now)).toMatch(
      /^Tomorrow/
    );
    expect(formatDue(new Date("2026-08-01T17:00:00").toISOString(), now)).toMatch(
      /^Sat/
    );
  });

  it("falls back to a date for anything past a week", () => {
    expect(formatDue(new Date("2026-09-15T17:00:00").toISOString(), now)).toMatch(
      /Sep/
    );
  });
});

describe("describeRecurrence", () => {
  it("says each of recurrence.rs's shapes in words", () => {
    expect(describeRecurrence("daily")).toBe("daily");
    expect(describeRecurrence("weekly:MO,TH")).toBe("weekly on Mon, Thu");
    expect(describeRecurrence("monthly:15")).toBe("monthly on the 15th");
    expect(describeRecurrence("quarterly:1")).toBe("quarterly on the 1st");
    expect(describeRecurrence("monthly:22")).toBe("monthly on the 22nd");
    expect(describeRecurrence("monthly:13")).toBe("monthly on the 13th");
  });

  /** The chip is the only place the rule shows at rest — a rule it can't read
   *  must still be visible, or the broken one is the one that disappears. */
  it("hands back a rule it doesn't recognise verbatim", () => {
    for (const rule of ["yearly", "weekly:XX", "monthly:0", "daily:3", "weekly"]) {
      expect(describeRecurrence(rule)).toBe(rule);
    }
  });

  it("is null for no rule", () => {
    expect(describeRecurrence(null)).toBeNull();
    expect(describeRecurrence("  ")).toBeNull();
  });
});

describe("isOverdue", () => {
  const now = new Date("2026-07-29T15:00:00");

  it("is false without a due date", () => {
    expect(isOverdue(baseTask, now)).toBe(false);
  });

  it("is true for a past due date on an open task", () => {
    expect(
      isOverdue({ ...baseTask, due_at: "2026-07-29T12:00:00.000Z" }, now)
    ).toBe(true);
  });

  it("is false once the task is closed — a done task is never overdue", () => {
    expect(
      isOverdue(
        { ...baseTask, due_at: "2026-07-29T12:00:00.000Z", status: "done" },
        now
      )
    ).toBe(false);
  });
});

describe("DateField conversions", () => {
  it("round-trips through the datetime-local format", () => {
    const iso = new Date("2026-08-01T17:00:00").toISOString();
    expect(localInputToIso(isoToLocalInput(iso))).toBe(iso);
  });

  it("maps empty/invalid both ways without throwing", () => {
    expect(isoToLocalInput(null)).toBe("");
    expect(isoToLocalInput("not a date")).toBe("");
    expect(localInputToIso("")).toBeNull();
    expect(localInputToIso("garbage")).toBeNull();
  });

  it("defaults to the next 5pm that is still in the future", () => {
    const morning = new Date("2026-07-29T09:00:00");
    expect(new Date(defaultDate(morning)).getDate()).toBe(29);

    // Past 5pm already — roll to tomorrow rather than creating an
    // instantly-overdue task.
    const evening = new Date("2026-07-29T19:00:00");
    const rolled = new Date(defaultDate(evening));
    expect(rolled.getDate()).toBe(30);
    expect(rolled.getHours()).toBe(17);
  });
});

describe("shortenPath", () => {
  it("abbreviates the home directory", () => {
    expect(shortenPath("/Users/me/Projects/cxtasks")).toBe(
      "~/Projects/cxtasks"
    );
  });

  it("abbreviates any user's home, not one hard-coded name", () => {
    expect(shortenPath("/Users/someone/code")).toBe("~/code");
    expect(shortenPath("/Users/someone")).toBe("~");
  });

  it("leaves other paths alone", () => {
    expect(shortenPath("/opt/homebrew/bin")).toBe("/opt/homebrew/bin");
  });
});

describe("repoName", () => {
  // The launch console's rail gives a node ~62-78px. `shortenPath` renders as
  // "~/Project…" there, which names no repo at all.
  it("takes the last segment", () => {
    expect(repoName("/Users/me/Projects/finance-app")).toBe("finance-app");
    expect(repoName("~/clients/acme")).toBe("acme");
  });

  it("ignores a trailing slash rather than returning empty", () => {
    expect(repoName("/Users/x/Projects/cxtasks/")).toBe("cxtasks");
  });

  // Nothing sensible to shorten — returning "" would render a blank rail node
  // that looks like an unset workspace.
  it("falls back to the input when there are no segments", () => {
    expect(repoName("/")).toBe("/");
    expect(repoName("cxtasks")).toBe("cxtasks");
  });
});

describe("parseDbTimestamp", () => {
  // The bug this exists to stop: `new Date("2026-07-29 14:03:22")` parses the
  // DB's bookkeeping format as LOCAL time, which shifts every age calculation
  // by the timezone offset — a comment posted a minute ago reads as "7h" in the
  // US and negative in Asia. Asserting against a fixed UTC instant catches it in
  // any timezone the suite happens to run in.
  const SPACE_FORM = "2026-07-29 14:03:22.481";
  const SAME_INSTANT_UTC = Date.parse("2026-07-29T14:03:22.481Z");

  it("reads the space-separated bookkeeping format as UTC, not local", () => {
    expect(parseDbTimestamp(SPACE_FORM)?.getTime()).toBe(SAME_INSTANT_UTC);
  });

  it("proves the fixture reproduces the bug it guards against", () => {
    // If this ever stops differing, the environment has changed and the
    // assertion above no longer proves anything — rewrite the fixture.
    expect(new Date(SPACE_FORM).getTime()).not.toBe(SAME_INSTANT_UTC);
  });

  it("leaves an already-ISO timestamp alone", () => {
    expect(parseDbTimestamp("2026-07-29T14:03:22.481Z")?.getTime()).toBe(
      SAME_INSTANT_UTC
    );
  });

  it("returns null rather than an Invalid Date", () => {
    expect(parseDbTimestamp(null)).toBeNull();
    expect(parseDbTimestamp("")).toBeNull();
    expect(parseDbTimestamp("garbage")).toBeNull();
  });
});

describe("formatAge", () => {
  const now = new Date("2026-07-29T12:00:00.000Z");
  const ago = (ms: number) => new Date(now.getTime() - ms);

  it("buckets from minutes up to weeks", () => {
    expect(formatAge(ago(30_000), now)).toBe("now");
    expect(formatAge(ago(12 * 60_000), now)).toBe("12m");
    expect(formatAge(ago(3 * 3_600_000), now)).toBe("3h");
    expect(formatAge(ago(2 * 86_400_000), now)).toBe("2d");
    expect(formatAge(ago(35 * 86_400_000), now)).toBe("5w");
  });

  it("accepts the DB's space-separated form directly", () => {
    expect(formatAge("2026-07-29 09:00:00.000", now)).toBe("3h");
  });

  it("is null for a missing timestamp", () => {
    expect(formatAge(null, now)).toBeNull();
  });
});

describe("formatShortId", () => {
  it("renders the speakable reference", () => {
    expect(formatShortId(42)).toBe("T42");
    expect(formatShortId(1)).toBe("T1");
  });

  it("is null for an unnumbered task rather than 'Tnull'", () => {
    expect(formatShortId(null)).toBeNull();
  });
});

describe("backgroundTier", () => {
  it("defaults to read-only", () => {
    expect(
      backgroundTier({ bg_writes_allowed: false, bg_shell_allowed: false })
    ).toBe("read-only");
  });

  it("reports the shell tier", () => {
    expect(
      backgroundTier({ bg_writes_allowed: false, bg_shell_allowed: true })
    ).toBe("shell");
  });

  // Writes imply a shell, so the pair is a precedence and not a combination.
  // Reading it the other way round would label a write-enabled task "shell,
  // no writes" — a label that understates what an unattended agent may do,
  // which is the one direction this must never fail in.
  it("lets writes outrank shell when both are set", () => {
    expect(
      backgroundTier({ bg_writes_allowed: true, bg_shell_allowed: true })
    ).toBe("write");
    expect(
      backgroundTier({ bg_writes_allowed: true, bg_shell_allowed: false })
    ).toBe("write");
  });

  it("mirrors the Rust ToolTier labels for every tier", () => {
    expect(TIER_LABELS["read-only"]).toBe("investigation");
    expect(TIER_LABELS.shell).toBe("commands and tests");
    expect(TIER_LABELS.write).toBe("edit workspace");
  });

  // The old middle label was "shell, no writes", which was false: a shell
  // writes, and that is exactly why this tier is checkpointed and repo-gated
  // like the write tier. A label that promises a boundary the code doesn't
  // enforce is worse than a vague one, so no tier label may claim it.
  it("never promises that a tier cannot write", () => {
    for (const label of Object.values(TIER_LABELS)) {
      expect(label).not.toMatch(/no writes|read.only|cannot write/i);
    }
  });

  // Same tiers for both agents, different enforcement — so the hints must be
  // present for every combination and must not be shared by accident.
  it("has a per-agent hint for every tier", () => {
    for (const executor of ["claude", "codex"] as const) {
      for (const tier of ["read-only", "shell", "write"] as const) {
        expect(TIER_HINTS[executor][tier]).toBeTruthy();
      }
    }
    expect(TIER_HINTS.codex["read-only"]).toContain("sandbox");
    expect(TIER_HINTS.claude["read-only"]).not.toContain("sandbox");
  });
});

describe("modelLabel", () => {
  it("strips the vendor prefix", () => {
    expect(modelLabel("claude-sonnet-5")).toBe("sonnet-5");
    expect(modelLabel("gpt-5.2-codex", "codex")).toBe("gpt-5.2-codex");
  });

  // Codex picks a model we never see. Naming one would be inventing it;
  // rendering nothing reads as a missing field on a panel whose whole job is
  // being trustworthy about what ran.
  it("says 'Codex default' rather than nothing when no model was recorded", () => {
    expect(modelLabel(null, "codex")).toBe("Codex default");
    expect(modelLabel("", "codex")).toBe("Codex default");
  });

  // Claude's path always records a model, so a blank one is genuinely unknown
  // and must not be labelled with a default that may not have been used.
  it("returns null for an unknown Claude model", () => {
    expect(modelLabel(null)).toBeNull();
    expect(modelLabel(null, "claude")).toBeNull();
  });
});

/**
 * The UI's half of `dispatch::reviewable_run`. If the two disagree the control
 * offers a review the backend refuses, or hides one it would have accepted.
 */
describe("lastHandoffRun", () => {
  function run(overrides: Partial<TaskRun> = {}): TaskRun {
    return {
      id: "r",
      task_id: "t1",
      mode: "background",
      status: "succeeded",
      started_at: "2026-08-01 10:00:00",
      finished_at: "2026-08-01 10:04:00",
      session_id: null,
      exit_code: 0,
      result_text: "a handoff",
      output_tokens: null,
      scratch_dir: null,
      input_tokens: null,
      cache_read_tokens: null,
      cache_write_tokens: null,
      model: null,
      executor: "claude",
      intent: "work",
      ...overrides,
    };
  }

  it("skips runs that are unfinished or left nothing", () => {
    expect(lastHandoffRun([])).toBeNull();
    expect(lastHandoffRun([run({ finished_at: null, result_text: null })])).toBeNull();
    expect(lastHandoffRun([run({ result_text: null })])).toBeNull();
    expect(lastHandoffRun([run({ result_text: "   " })])).toBeNull();
  });

  // A timed-out run's partial handoff is genuinely reviewable — and often the
  // one most worth a second pair of eyes. Filtering to `succeeded` here would
  // refuse it while the backend accepts it.
  it("accepts a partial handoff from a run that did not succeed", () => {
    expect(lastHandoffRun([run({ status: "timeout", result_text: "got halfway" })])?.id).toBe("r");
  });

  /**
   * The list arrives newest-first from SQL. This fixture's `started_at` values
   * are deliberately ASCENDING while the array order is authoritative, so a
   * well-meaning `[...runs].sort()` fails here — the same mistake invariant 18
   * records as having already shipped once on the Rust side.
   */
  it("trusts the list's newest-first order rather than re-sorting it", () => {
    const newest = run({ id: "newest", started_at: "2026-08-01 09:00:00" });
    const older = run({ id: "older", started_at: "2026-08-01 11:00:00" });
    expect(lastHandoffRun([newest, older])?.id).toBe("newest");
  });
});

describe("otherExecutor", () => {
  it("is what a review preselects — never the agent that wrote the handoff", () => {
    expect(otherExecutor("claude")).toBe("codex");
    expect(otherExecutor("codex")).toBe("claude");
  });
});

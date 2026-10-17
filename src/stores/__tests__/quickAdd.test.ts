import { describe, expect, it } from "vitest";
import { parseDueToken, parseQuickAdd } from "@/stores/taskStore";

describe("parseQuickAdd", () => {
  it("keeps a plain title intact", () => {
    expect(parseQuickAdd("buy guitar strings")).toEqual({
      title: "buy guitar strings",
      project: null,
      priority: 3,
      due_at: null,
      tags: [],
    });
  });

  it("extracts #project without eating it from the title twice", () => {
    const parsed = parseQuickAdd("fix the webhook #cxventures");
    expect(parsed?.project).toBe("cxventures");
    expect(parsed?.title).toBe("fix the webhook");
  });

  it("inherits the active project when no #project is typed", () => {
    expect(parseQuickAdd("fix the empty state", "pocketbuddy")?.project).toBe(
      "pocketbuddy"
    );
  });

  it("lets an explicit #project override the active project", () => {
    expect(parseQuickAdd("ship it #cxtasks", "pocketbuddy")?.project).toBe(
      "cxtasks"
    );
  });

  it("extracts !priority", () => {
    expect(parseQuickAdd("ship it !3")?.priority).toBe(3);
    expect(parseQuickAdd("ship it !0")?.priority).toBe(0);
  });

  it("leaves a bare exclamation in the title alone", () => {
    // `!` is punctuation people actually type — only `!0`–`!3` is a priority.
    const parsed = parseQuickAdd("ship it!");
    expect(parsed?.title).toBe("ship it!");
    expect(parsed?.priority).toBe(3);
    expect(parseQuickAdd("do it !9")?.title).toBe("do it !9");
  });

  it("handles all three tokens at once, in any position", () => {
    const parsed = parseQuickAdd("#cxtasks !2 write the runner @tomorrow");
    expect(parsed?.project).toBe("cxtasks");
    expect(parsed?.priority).toBe(2);
    expect(parsed?.title).toBe("write the runner");
    expect(parsed?.due_at).toBeTruthy();
  });

  it("reads an unrecognized @token as a person, not a date", () => {
    // `email @dana` used to keep the token in the title so the recipient
    // wasn't lost; now the recipient is kept as the person tag `@dana`, which
    // is what makes "what's open with Dana" answerable.
    const parsed = parseQuickAdd("email @dana about the audit");
    expect(parsed?.title).toBe("email about the audit");
    expect(parsed?.tags).toEqual(["@dana"]);
    expect(parsed?.due_at).toBeNull();
  });

  it("returns null when only modifiers were typed", () => {
    expect(parseQuickAdd("#project !2")).toBeNull();
    expect(parseQuickAdd("   ")).toBeNull();
  });
});

describe("parseDueToken", () => {
  const wed = new Date("2026-07-29T15:00:00"); // a Wednesday, local

  it("resolves today and tomorrow to 5pm local", () => {
    const today = new Date(parseDueToken("today", wed)!);
    expect(today.getHours()).toBe(17);
    expect(today.getDate()).toBe(29);

    const tomorrow = new Date(parseDueToken("tomorrow", wed)!);
    expect(tomorrow.getDate()).toBe(30);
  });

  it("resolves a weekday to its next occurrence", () => {
    const fri = new Date(parseDueToken("fri", wed)!);
    expect(fri.getDay()).toBe(5);
    expect(fri.getDate()).toBe(31);
  });

  it("pushes same-weekday a full week out, never to today", () => {
    const nextWed = new Date(parseDueToken("wed", wed)!);
    expect(nextWed.getDay()).toBe(3);
    expect(nextWed.getDate()).toBe(5); // Aug 5, not Jul 29
    expect(nextWed.getTime()).toBeGreaterThan(wed.getTime());
  });

  it("rejects anything else", () => {
    expect(parseDueToken("dana", wed)).toBeNull();
    expect(parseDueToken("next-friday", wed)).toBeNull();
  });
});

describe("parseQuickAdd — people", () => {
  it("turns an @word that is not a date into a person tag", () => {
    const parsed = parseQuickAdd("follow up with @Sam about the seminar");
    expect(parsed?.title).toBe("follow up with about the seminar");
    expect(parsed?.tags).toEqual(["@sam"]);
    expect(parsed?.due_at).toBeNull();
  });

  it("keeps the date words as dates, so @fri is still Friday", () => {
    const parsed = parseQuickAdd("ask @dana for a testimonial @fri");
    expect(parsed?.tags).toEqual(["@dana"]);
    expect(parsed?.due_at).not.toBeNull();
  });

  it("drops trailing punctuation from a name and never repeats one", () => {
    const parsed = parseQuickAdd("email @andy, then @andy again");
    expect(parsed?.tags).toEqual(["@andy"]);
  });

  it("leaves a bare @ in the title — it is not a person", () => {
    expect(parseQuickAdd("email @ noon")?.title).toBe("email @ noon");
    expect(parseQuickAdd("email @ noon")?.tags).toEqual([]);
  });
});

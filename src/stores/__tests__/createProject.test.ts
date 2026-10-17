/**
 * Creating an area or project lands you in it.
 *
 * The sidebar only draws a node with open work, or the one you are standing
 * in. A node that was just made has no work, so if creating it did not also
 * select it, the row you just typed would appear nowhere — the field closes
 * and nothing seems to have happened. Selecting it is also what makes the
 * next capture file there.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useTaskStore } from "@/stores/taskStore";
import type { Project } from "@/types/task";

const created: Project = {
  id: "p-1",
  // What the server STORED, not what was typed: `normalize` trims and
  // collapses whitespace, and the row only matches `activeProject` by the
  // stored name.
  name: "gov certs",
  color: null,
  icon: null,
  sort_order: 0,
  archived: false,
  open_count: 0,
  health: null,
  kind: "project",
  parent: "cxventures",
  target_date: null,
  closed_at: null,
};

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: {
      list: vi.fn(async () => []),
      search: vi.fn(async () => []),
      subtasks: vi.fn(async () => []),
      projects: vi.fn(async () => []),
      counts: vi.fn(async () => ({ inbox: 0, today: 0, upcoming: 0, in_review: 0 })),
      countdowns: vi.fn(async () => ({})),
    },
    runs: { list: vi.fn(async () => []) },
    tags: {
      list: vi.fn(async () => []),
      forTask: vi.fn(async () => []),
      byTask: vi.fn(async () => ({})),
    },
    deps: { blockedIds: vi.fn(async () => []) },
    comments: { list: vi.fn(async () => []) },
    attachments: { list: vi.fn(async () => []) },
    sections: { list: vi.fn(async () => []) },
    projects: {
      list: vi.fn(async () => []),
      board: vi.fn(async () => []),
      create: vi.fn(async () => created),
      repos: { list: vi.fn(async () => []) },
    },
    tripwires: { runtime: vi.fn(async () => ({})), scripts: vi.fn(async () => []) },
  },
}));

beforeEach(() => {
  vi.clearAllMocks();
  useTaskStore.setState({
    activeProject: null,
    view: "today",
    query: "",
    filters: [],
    activeTags: [],
  } as never);
});

describe("createProject", () => {
  it("scopes the list to the node it just made, by its stored name", async () => {
    const err = await useTaskStore
      .getState()
      .createProject("  gov   certs ", "project", "cxventures");

    expect(err).toBeNull();
    const s = useTaskStore.getState();
    expect(s.activeProject).toBe("gov certs");
    expect(s.view).toBe("all");
  });

  it("stays where it was when the server refuses", async () => {
    const { api } = await import("@/lib/tauri");
    // Tauri rejects with the error's STRING, not an Error object.
    vi.mocked(api.projects.create).mockRejectedValueOnce(
      "Invalid input: a project called 'gov certs' already exists"
    );

    const err = await useTaskStore
      .getState()
      .createProject("gov certs", "project", "cxventures");

    expect(err).toBe("a project called 'gov certs' already exists");
    expect(useTaskStore.getState().activeProject).toBeNull();
  });
});

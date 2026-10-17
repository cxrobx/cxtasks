import { beforeEach, expect, it, vi } from "vitest";
import { api } from "@/lib/tauri";
import { useTaskStore } from "@/stores/taskStore";
import type { CaptureAssessment, Task } from "@/types/task";

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: { create: vi.fn() },
    tags: { setForTask: vi.fn(async () => {}) },
  },
}));

const task = { id: "created", title: "Ship", project: "Chosen" } as Task;
const suggestion: CaptureAssessment = {
  project: "Suggested", priority: 3, status: "todo", tags: ["suggested"],
  needs_clarification: 0, confidence: 1, too_vague: false, engine: "jev",
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.tasks.create).mockResolvedValue(task);
  useTaskStore.setState({
    activeProject: null,
    tasks: [task],
    refresh: vi.fn(async () => {}),
    select: vi.fn(async () => {}),
    pushUndo: vi.fn(),
  });
});

it.each([0, 1, 2, 3])("preserves explicit project and !%i priority against enrichment", async (priority) => {
  await useTaskStore.getState().quickAdd(`Ship #Chosen !${priority}`, suggestion);
  expect(api.tasks.create).toHaveBeenCalledExactlyOnceWith({
    title: "Ship", project: "Chosen", priority, due_at: null, status: "todo",
  });
});

it("preserves the active project against enrichment", async () => {
  useTaskStore.setState({ activeProject: "Chosen" });
  await useTaskStore.getState().quickAdd("Ship", suggestion);
  expect(api.tasks.create).toHaveBeenCalledExactlyOnceWith({
    title: "Ship", project: "Chosen", priority: 3, due_at: null, status: "todo",
  });
});

it("still enriches project and priority when neither is explicit", async () => {
  await useTaskStore.getState().quickAdd("Ship", { ...suggestion, priority: 1 });
  expect(api.tasks.create).toHaveBeenCalledExactlyOnceWith({
    title: "Ship", project: "Suggested", priority: 1, due_at: null, status: "todo",
  });
});

it("files an unenriched capture at the LOWEST priority, not P0", async () => {
  // A timed-out or failed assessment passes no suggestion. P0 is the highest
  // priority (invariant 9), so defaulting to 0 made every such capture urgent.
  await useTaskStore.getState().quickAdd("Ship", null);
  expect(api.tasks.create).toHaveBeenCalledExactlyOnceWith({
    title: "Ship", project: null, priority: 3, due_at: null,
  });
});

import { beforeEach, expect, it, vi } from "vitest";
import { api } from "@/lib/tauri";
import { useTaskStore } from "@/stores/taskStore";

vi.mock("@/lib/tauri", () => ({ api: {
  comments: { add: vi.fn(), addWithAttachments: vi.fn() },
} }));
const select = vi.fn(async () => {});
beforeEach(() => {
  vi.clearAllMocks();
  useTaskStore.setState({ selectedId: "t1", select, error: null });
});

it("sends image-only draft bytes through the atomic comment attachment command", async () => {
  const images = [[137, 80, 78, 71]];
  expect(await useTaskStore.getState().addComment("t1", "", [], images)).toBe(true);
  expect(api.comments.addWithAttachments).toHaveBeenCalledWith("t1", "", [], images);
  expect(api.comments.add).not.toHaveBeenCalled();
  expect(select).toHaveBeenCalledWith("t1");
});

it("reports import errors without claiming the comment was posted", async () => {
  vi.mocked(api.comments.addWithAttachments).mockRejectedValueOnce("Image larger than 25 MB");
  expect(await useTaskStore.getState().addComment("t1", "context", [], [[1]])).toBe(false);
  expect(useTaskStore.getState().error).toBe("Image larger than 25 MB");
  expect(select).not.toHaveBeenCalled();
});

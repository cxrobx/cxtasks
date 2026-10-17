import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { CommentComposer } from "@/components/tasks/TaskActivity";
import { useTaskStore } from "@/stores/taskStore";
import { api } from "@/lib/tauri";
import type { Task } from "@/types/task";

vi.mock("@/lib/tauri", () => ({ api: { attachments: { readClipboard: vi.fn(), tryReadClipboard: vi.fn() } } }));
vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (path: string) => path }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
const task = { id: "task-clipboard" } as Task;
const post = vi.fn();
const bytes = [137, 80, 78, 71];

beforeEach(() => {
  vi.clearAllMocks();
  URL.createObjectURL = vi.fn(() => "blob:preview");
  URL.revokeObjectURL = vi.fn();
  useTaskStore.setState({ addComment: post, error: null });
  vi.mocked(api.attachments.readClipboard).mockResolvedValue(bytes);
  vi.mocked(api.attachments.tryReadClipboard).mockResolvedValue(bytes);
  post.mockResolvedValue(true);
});

describe("comment clipboard images", () => {
  it("stages image paste with the draft and posts both only on request", async () => {
    render(<CommentComposer task={task} />);
    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "Screenshot context" } });
    expect(fireEvent.paste(input, { clipboardData: { items: [{ type: "image/png" }] } })).toBe(false);
    await screen.findByAltText("Clipboard image");
    expect(post).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Post" }));
    await waitFor(() => expect(post).toHaveBeenCalledWith(task.id, "Screenshot context", [], [bytes]));
    await waitFor(() => expect(screen.queryByAltText("Clipboard image")).toBeNull());
    expect(input).toHaveValue("");
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview");
  });

  it("allows image-only comments and removal before posting", async () => {
    render(<CommentComposer task={task} />);
    fireEvent.click(screen.getByRole("button", { name: "Paste image" }));
    await screen.findByAltText("Clipboard image");
    expect(screen.getByRole("button", { name: "Post" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Remove clipboard image" }));
    expect(screen.queryByRole("button", { name: "Post" })).toBeNull();
    expect(post).not.toHaveBeenCalled();
  });

  it("reads the native image when WKWebView exposes no paste items", async () => {
    render(<CommentComposer task={task} />);
    fireEvent.paste(screen.getByRole("textbox"), { clipboardData: { items: [] } });
    await screen.findByAltText("Clipboard image");
    expect(api.attachments.tryReadClipboard).toHaveBeenCalledOnce();
    expect(post).not.toHaveBeenCalled();
  });

  it("handles Cmd+V even when WebKit does not dispatch a paste event", async () => {
    render(<CommentComposer task={task} />);
    fireEvent.keyDown(screen.getByRole("textbox"), { key: "v", metaKey: true });
    await screen.findByAltText("Clipboard image");
    expect(api.attachments.tryReadClipboard).toHaveBeenCalledOnce();
    expect(post).not.toHaveBeenCalled();
  });

  it("does not import twice when a paste event follows Cmd+V", async () => {
    render(<CommentComposer task={task} />);
    const input = screen.getByRole("textbox");
    fireEvent.keyDown(input, { key: "v", metaKey: true });
    fireEvent.paste(input, { clipboardData: { items: [{ type: "image/png" }] } });
    await screen.findByAltText("Clipboard image");
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(api.attachments.readClipboard).toHaveBeenCalledOnce();
    expect(api.attachments.tryReadClipboard).not.toHaveBeenCalled();
    expect(screen.getAllByAltText("Clipboard image")).toHaveLength(1);
  });

  it("keeps a text-only clipboard quiet when WebKit omits its paste event", async () => {
    vi.mocked(api.attachments.tryReadClipboard).mockResolvedValue(null);
    render(<CommentComposer task={task} />);
    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "Keep this text" } });
    fireEvent.keyDown(input, { key: "v", metaKey: true });
    await waitFor(() => expect(api.attachments.tryReadClipboard).toHaveBeenCalledOnce());
    expect(input).toHaveValue("Keep this text");
    expect(useTaskStore.getState().error).toBeNull();
    expect(screen.queryByAltText("Clipboard image")).toBeNull();
  });

  it("keeps text paste unchanged", () => {
    render(<CommentComposer task={task} />);
    expect(fireEvent.paste(screen.getByRole("textbox"), { clipboardData: { items: [{ type: "text/plain" }] } })).toBe(true);
    expect(api.attachments.readClipboard).not.toHaveBeenCalled();
    expect(api.attachments.tryReadClipboard).not.toHaveBeenCalled();
  });

  it("shows clipboard errors and keeps the draft", async () => {
    vi.mocked(api.attachments.readClipboard).mockRejectedValue("No image in the clipboard");
    render(<CommentComposer task={task} />);
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Keep this" } });
    fireEvent.click(screen.getByRole("button", { name: "Paste image" }));
    await waitFor(() => expect(useTaskStore.getState().error).toBe("No image in the clipboard"));
    expect(screen.getByRole("textbox")).toHaveValue("Keep this");
  });

  it("retains staged images and text after a failed post", async () => {
    post.mockResolvedValue(false);
    render(<CommentComposer task={task} />);
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Retry this" } });
    fireEvent.click(screen.getByRole("button", { name: "Paste image" }));
    await screen.findByAltText("Clipboard image");
    fireEvent.click(screen.getByRole("button", { name: "Post" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Paste image" })).toBeEnabled());
    expect(screen.getByAltText("Clipboard image")).toBeTruthy();
    expect(screen.getByRole("textbox")).toHaveValue("Retry this");
  });
});

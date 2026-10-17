import { useRef } from "react";
import { beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useKeyboardShortcuts } from "@/hooks/useKeyboardShortcuts";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { Task } from "@/types/task";

vi.mock("@/lib/tauri", () => ({ api: {} }));
const paste = vi.fn(async () => true);

function Harness() {
  const quickAdd = useRef<HTMLInputElement>(null);
  const search = useRef<HTMLInputElement>(null);
  useKeyboardShortcuts(quickAdd, search);
  return <><input aria-label="Search" ref={search} /><div data-image-paste tabIndex={0}>Notes preview</div></>;
}

beforeEach(() => {
  vi.clearAllMocks();
  useTaskStore.setState({ selectedId: "t1", tasks: [{ id: "t1" } as Task], pasteNoteImage: paste });
  useUiStore.setState({ settingsOpen: false });
});

it("attaches Cmd+V to the selected task when focus is outside an editor", async () => {
  render(<Harness />);
  fireEvent.keyDown(document.body, { key: "v", metaKey: true });
  await waitFor(() => expect(paste).toHaveBeenCalledWith("t1", true));
});

it("attaches a native image paste to the selected task once", async () => {
  render(<Harness />);
  fireEvent.keyDown(document.body, { key: "v", metaKey: true });
  expect(fireEvent.paste(document.body, { clipboardData: { items: [{ type: "image/png" }] } })).toBe(false);
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(paste).toHaveBeenCalledExactlyOnceWith("t1", false);
});

it("leaves editors and Notes' own paste handler alone", async () => {
  render(<Harness />);
  fireEvent.keyDown(screen.getByRole("textbox"), { key: "v", metaKey: true });
  fireEvent.keyDown(screen.getByText("Notes preview"), { key: "v", metaKey: true });
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(paste).not.toHaveBeenCalled();
});

it("does not attach to an archived task or while Settings is open", async () => {
  render(<Harness />);
  useTaskStore.setState({ tasks: [{ id: "t1", archived_at: "2026-10-01" } as Task] });
  fireEvent.paste(document.body, { clipboardData: { items: [{ type: "image/png" }] } });
  useTaskStore.setState({ tasks: [{ id: "t1" } as Task] });
  useUiStore.setState({ settingsOpen: true });
  fireEvent.keyDown(document.body, { key: "v", metaKey: true });
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(paste).not.toHaveBeenCalled();
});

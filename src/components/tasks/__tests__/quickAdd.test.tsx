import { createRef } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QuickAdd } from "../QuickAdd";
import { useTaskStore } from "@/stores/taskStore";
import { api } from "@/lib/tauri";
import type { CaptureAssessment } from "@/types/task";

vi.mock("@/lib/tauri", () => ({
  api: { jev: { assessCapture: vi.fn(async () => null) } },
}));
const capture = vi.fn(async () => ({ id: "new", project: null } as never));
let resize: ResizeObserverCallback;
let resizeFrame: FrameRequestCallback;

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.jev.assessCapture).mockResolvedValue(null);
  capture.mockResolvedValue({ id: "new", project: null } as never);
  vi.stubGlobal("requestAnimationFrame", vi.fn((callback: FrameRequestCallback) => {
    resizeFrame = callback;
    return 1;
  }));
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: ResizeObserverCallback) { resize = callback; }
    observe() {}
    disconnect() {}
  });
  useTaskStore.setState({ quickAdd: capture, activeProject: null, projects: [], allTags: [] });
});

afterEach(() => vi.useRealTimers());

const suggestion: CaptureAssessment = {
  project: "Suggested", priority: 3, status: "todo", tags: [],
  needs_clarification: 0, confidence: 1, too_vague: false, engine: "jev",
};

it("bounds a never-resolving assessment and permits another capture afterward", async () => {
  vi.useFakeTimers();
  vi.mocked(api.jev.assessCapture).mockImplementation(() => new Promise(() => {}));
  render(<QuickAdd />);
  const input = screen.getByRole("textbox");
  fireEvent.change(input, { target: { value: "Ship #Chosen !0" } });
  input.focus();
  // Batch events before React commits busy; the ref must guard this too.
  act(() => {
    fireEvent.submit(screen.getByRole("form"));
    fireEvent.submit(screen.getByRole("form"));
    fireEvent.keyDown(input, { key: "Enter" });
  });
  // Read-only, not disabled: disabling the focused field would drop focus
  // and break type-Enter-type-Enter.
  expect(input).toHaveAttribute("readonly");
  expect(input).toHaveFocus();
  expect(screen.getByRole("button")).toBeDisabled();
  await act(() => vi.advanceTimersByTimeAsync(999));
  expect(capture).not.toHaveBeenCalled();
  await act(() => vi.advanceTimersByTimeAsync(1));
  expect(capture).toHaveBeenCalledExactlyOnceWith("Ship #Chosen !0", null);
  expect(input).not.toHaveAttribute("readonly");
  expect(input).toHaveFocus();
  fireEvent.change(input, { target: { value: "Next task" } });
  expect(screen.getByRole("button", { name: "Create task" })).toBeEnabled();
  fireEvent.submit(screen.getByRole("form"));
  await act(() => vi.advanceTimersByTimeAsync(1000));
  expect(capture).toHaveBeenCalledTimes(2);
});

it.each(["resolve", "reject"])("ignores late assessment %s without changing or duplicating capture", async (outcome) => {
  vi.useFakeTimers();
  let resolve!: (value: CaptureAssessment) => void;
  let reject!: (reason: Error) => void;
  vi.mocked(api.jev.assessCapture).mockReturnValue(new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  }));
  render(<QuickAdd />);
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Original" } });
  fireEvent.submit(screen.getByRole("form"));
  await act(() => vi.advanceTimersByTimeAsync(1000));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "New draft" } });
  await act(async () => {
    if (outcome === "resolve") resolve(suggestion);
    else reject(new Error("provider failed late"));
  });
  expect(capture).toHaveBeenCalledExactlyOnceWith("Original", null);
  expect(screen.getByRole("textbox")).toHaveValue("New draft");
});

it("creates immediately after provider failure and clears busy after a failed save", async () => {
  vi.mocked(api.jev.assessCapture).mockRejectedValue(new Error("TLS failure"));
  capture.mockResolvedValueOnce(null as never);
  render(<QuickAdd />);
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Retry me" } });
  fireEvent.submit(screen.getByRole("form"));
  await waitFor(() => expect(screen.getByRole("button", { name: "Create task" })).toBeEnabled());
  expect(capture).toHaveBeenCalledExactlyOnceWith("Retry me", null);
  expect(screen.getByRole("textbox")).toHaveValue("Retry me");
});

it("uses suggestions that arrive within the capture budget", async () => {
  vi.mocked(api.jev.assessCapture).mockResolvedValue(suggestion);
  render(<QuickAdd />);
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Fast" } });
  fireEvent.submit(screen.getByRole("form"));
  await waitFor(() => expect(capture).toHaveBeenCalledExactlyOnceWith("Fast", suggestion));
});

it("grows to the content height, reflows on width changes, and shrinks when cleared", () => {
  render(<QuickAdd />);
  const input = screen.getByRole("textbox", { name: "Task title" });
  let height = 24;
  Object.defineProperty(input, "scrollHeight", { get: () => height });
  height = 72;
  fireEvent.change(input, { target: { value: "A longer task title" } });
  expect(input.style.height).toBe("72px");
  height = 96;
  const widthChange = [{ contentRect: { width: 200 } } as ResizeObserverEntry];
  resize(widthChange, {} as ResizeObserver);
  expect(input.style.height).toBe("72px");
  resizeFrame(0);
  expect(input.style.height).toBe("96px");
  // The resulting height notification must not start another resize loop.
  resize(widthChange, {} as ResizeObserver);
  expect(requestAnimationFrame).toHaveBeenCalledTimes(1);
  height = 24;
  fireEvent.keyDown(input, { key: "Escape" });
  expect(input).toHaveValue("");
  expect(input.style.height).toBe("24px");
});

it("preserves forwarded focus and submits with Enter, but not Shift+Enter or IME composition", async () => {
  const ref = createRef<HTMLTextAreaElement>();
  render(<QuickAdd ref={ref} />);
  const input = screen.getByRole("textbox", { name: "Task title" });
  ref.current?.focus();
  expect(input).toHaveFocus();
  fireEvent.change(input, { target: { value: "First line\nSecond line" } });
  expect(fireEvent.keyDown(input, { key: "Enter", shiftKey: true })).toBe(true);
  fireEvent.keyDown(input, { key: "Enter", isComposing: true });
  expect(capture).not.toHaveBeenCalled();
  expect(fireEvent.keyDown(input, { key: "Enter" })).toBe(false);
  await waitFor(() => expect(capture).toHaveBeenCalledWith("First line\nSecond line", null));
  await waitFor(() => expect(input).toHaveValue(""));
});

it("still submits using the Create task button", async () => {
  render(<QuickAdd />);
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "New task" } });
  fireEvent.click(screen.getByRole("button", { name: "Create task" }));
  await waitFor(() => expect(capture).toHaveBeenCalledWith("New task", null));
});

/**
 * Archive and Trash as one destination with two segments.
 *
 * This control is load-bearing in a way the old two-row sidebar wasn't: it is
 * now the ONLY way to reach the Trash. `search_tasks` deliberately excludes
 * trashed tasks (archived ones still match), so if these tabs stop rendering,
 * deleted work becomes unreachable from the UI entirely — with nothing on
 * screen to say so.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { TaskList } from "@/components/tasks/TaskList";
import { useTaskStore } from "@/stores/taskStore";

const setView = vi.fn(async (_v: string) => {});

vi.mock("@/lib/tauri", () => ({
  api: {
    tasks: { complete: vi.fn(), update: vi.fn(), trash: vi.fn() },
    menu: { showForTask: vi.fn() },
  },
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn(async () => false) }));

beforeEach(() => {
  vi.clearAllMocks();
  useTaskStore.setState({
    tasks: [],
    sections: [],
    checkedIds: [],
    countdowns: {},
    loading: false,
    setView,
  } as never);
});

describe("the Archive/Trash segments", () => {
  it("offers both, with the current one marked", () => {
    useTaskStore.setState({ view: "archive" } as never);
    render(<TaskList />);

    expect(screen.getByRole("tab", { name: "Archived" })).toHaveAttribute(
      "aria-selected",
      "true"
    );
    expect(screen.getByRole("tab", { name: "Trash" })).toHaveAttribute(
      "aria-selected",
      "false"
    );
  });

  it("is the only door to the Trash, so it must switch views", () => {
    useTaskStore.setState({ view: "archive" } as never);
    render(<TaskList />);

    fireEvent.click(screen.getByRole("tab", { name: "Trash" }));
    expect(setView).toHaveBeenCalledWith("trash");
  });

  it("switches back to the Archive", () => {
    useTaskStore.setState({ view: "trash" } as never);
    render(<TaskList />);

    expect(screen.getByRole("tab", { name: "Trash" })).toHaveAttribute(
      "aria-selected",
      "true"
    );
    fireEvent.click(screen.getByRole("tab", { name: "Archived" }));
    expect(setView).toHaveBeenCalledWith("archive");
  });

  it("renders on an EMPTY trash too — the one case you'd lose the way out", () => {
    // The empty state takes a different return path in TaskList. If the
    // segments only rendered alongside rows, emptying the Trash would strip
    // the control that got you there and strand you in a blank view.
    useTaskStore.setState({ view: "trash", tasks: [] } as never);
    render(<TaskList />);

    expect(screen.getByRole("tab", { name: "Archived" })).toBeInTheDocument();
    expect(screen.getByText("Nothing here.")).toBeInTheDocument();
  });

  it("says what each one promises, since they differ in whether work survives", () => {
    useTaskStore.setState({ view: "trash" } as never);
    const { unmount } = render(<TaskList />);
    expect(screen.getByText(/Removed after 30 days/)).toBeInTheDocument();
    unmount();

    useTaskStore.setState({ view: "archive" } as never);
    render(<TaskList />);
    expect(screen.getByText(/Never deleted/)).toBeInTheDocument();
  });

  it("keeps the segments out of the working views", () => {
    useTaskStore.setState({ view: "today" } as never);
    render(<TaskList />);
    expect(screen.queryByRole("tab", { name: "Trash" })).toBeNull();
  });
});

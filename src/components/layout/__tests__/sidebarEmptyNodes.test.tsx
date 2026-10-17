/**
 * Only an area or project with open work is drawn — until "Show hidden".
 *
 * The sidebar is a filter over open work, so a node with nothing open in it is
 * a row that scopes the list to nothing. Right-clicking the Areas label →
 * "Show hidden" brings the empty ones back (persisted). Three things keep a
 * node on screen regardless: standing in it (the clause tags carry, so
 * completing the last task does not pull the highlighted row out from under
 * you), holding something that is drawn (an active project needs its parent),
 * and having been created this session. The "move to…" targets must NOT follow
 * the rule.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { Sidebar } from "@/components/layout/Sidebar";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { Project } from "@/types/task";

vi.mock("@/lib/tauri", () => ({
  api: {
    tags: { usage: vi.fn(async () => 0) },
    budget: { status: vi.fn(async () => null) },
    runner: { isInstalled: vi.fn(async () => false) },
    runs: { recent: vi.fn(async () => []) },
  },
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn(async () => false) }));

function node(
  name: string,
  overrides: Partial<Project> & { kind: Project["kind"] }
): Project {
  return {
    id: `p-${name}`,
    name,
    color: null,
    icon: null,
    sort_order: 0,
    archived: false,
    open_count: 0,
    health: null,
    parent: null,
    target_date: null,
    closed_at: null,
    ...overrides,
  };
}

/**
 * CXVentures holds one live project and one finished one; Music is an area
 * with nothing open; Chores is an area whose only open work is its own;
 * Taxes is a top-level project that is done.
 */
const RECORDS = [
  node("cxventures", { kind: "area", open_count: 4 }),
  node("lsa launch", { kind: "project", parent: "cxventures", open_count: 4 }),
  node("gov certs", { kind: "project", parent: "cxventures", open_count: 0 }),
  node("music", { kind: "area", open_count: 0 }),
  node("chores", { kind: "area", open_count: 2 }),
  node("laundry", { kind: "project", parent: "chores", open_count: 0 }),
  node("taxes", { kind: "project", open_count: 0 }),
];

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useTaskStore.setState({
    projectRecords: RECORDS,
    activeProject: null,
    allTags: [],
    activeTags: [],
    view: "today",
    query: "",
    filters: [],
    setProject: vi.fn(async () => {}),
  } as never);
  useUiStore.setState({ areasCollapsed: false, collapsedAreas: [], showHidden: false });
});

/** The area/project rows on screen, by name, in the order drawn. */
function rows(): string[] {
  return screen
    .getAllByRole("button")
    .map((b) => b.textContent ?? "")
    .map((t) => RECORDS.find((p) => t.startsWith(p.name))?.name)
    .filter((n): n is string => n !== undefined);
}

describe("an empty area or project", () => {
  it("draws neither an empty area nor an empty project", () => {
    render(<Sidebar />);

    // gov certs is finished, taxes is done and music holds nothing — none is a
    // place you can go to find work.
    expect(rows()).toEqual(["cxventures", "lsa launch", "chores"]);
    // And with no visible top-level project, the "Projects" heading over them
    // goes too — a heading over nothing is a question with no answer.
    expect(screen.queryByText("Projects", { selector: "li" })).not.toBeInTheDocument();
  });

  it("stays while you are standing in it", () => {
    // Complete the last open task in the project you are in and the row must
    // not vanish from under the highlight — the nav would show nowhere.
    useTaskStore.setState({ activeProject: "gov certs" } as never);

    render(<Sidebar />);

    expect(rows()).toEqual(["cxventures", "lsa launch", "gov certs", "chores"]);
    expect(screen.getByRole("button", { name: /^gov certs/ })).toHaveAttribute(
      "aria-current",
      "page"
    );
  });

  it("keeps an empty area on screen when the project you are in is under it", () => {
    // Music is at zero, and so is everything under it — but the project you
    // are standing in needs a parent to nest under.
    useTaskStore.setState({
      projectRecords: [
        ...RECORDS,
        node("mixtape", { kind: "project", parent: "music", open_count: 0 }),
      ],
      activeProject: "mixtape",
    } as never);

    render(<Sidebar />);

    expect(screen.getByRole("button", { name: /^music/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^mixtape/ })).toHaveAttribute(
      "aria-current",
      "page"
    );
    expect(rows()).not.toContain("taxes");
  });

  it("gives an area no chevron when its only open work is its own", () => {
    // Chores has two open tasks of its own and one finished project. There is
    // nothing under it to put away, so it keeps its folder rather than growing
    // a disclosure that hides nothing.
    render(<Sidebar />);

    expect(
      screen.queryByRole("button", { name: /projects in chores$/ })
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /projects in cxventures$/ })).toBeInTheDocument();
  });

  it("shows the Projects heading for a top-level project with open work", () => {
    useTaskStore.setState({
      projectRecords: RECORDS.map((p) =>
        p.name === "taxes" ? { ...p, open_count: 1 } : p
      ),
    } as never);

    render(<Sidebar />);

    expect(rows()).toContain("taxes");
    expect(screen.getByText("Projects", { selector: "li" })).toBeInTheDocument();
  });
});

describe("Show hidden", () => {
  it("brings back every empty area and project, and remembers the choice", async () => {
    render(<Sidebar />);

    fireEvent.contextMenu(screen.getByRole("button", { name: /^Areas/ }));
    fireEvent.click(await screen.findByRole("menuitem", { name: /Show hidden/ }));

    await waitFor(() =>
      expect(rows()).toEqual(["cxventures", "lsa launch", "gov certs", "music", "chores", "laundry", "taxes"])
    );
    expect(localStorage.getItem("cxtasks.showHidden")).toBe("1");
  });

  it("keeps a click on the label collapsing the section", () => {
    render(<Sidebar />);
    const header = screen.getByRole("button", { name: /^Areas/ });

    fireEvent.click(header);

    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(useUiStore.getState().showHidden).toBe(false);
  });
});

describe("a node created this session", () => {
  it("stays drawn after you click away from it", async () => {
    const createProject = vi.fn(async (name: string) => {
      useTaskStore.setState({
        projectRecords: [...RECORDS, node(name, { kind: "area", open_count: 0 })],
        activeProject: name,
      } as never);
      return null;
    });
    useTaskStore.setState({ createProject } as never);
    render(<Sidebar />);

    fireEvent.click(screen.getByRole("button", { name: "New area" }));
    const field = screen.getByPlaceholderText("Area name…");
    fireEvent.change(field, { target: { value: "Fresh" } });
    fireEvent.keyDown(field, { key: "Enter" });
    const fresh = () => screen.queryByRole("button", { name: /^Fresh/ });
    await waitFor(() => expect(fresh()).toBeInTheDocument());

    // Navigate elsewhere: it is empty and no longer active, but it was just made.
    useTaskStore.setState({ activeProject: null } as never);
    await waitFor(() => expect(fresh()).toBeInTheDocument());
  });
});

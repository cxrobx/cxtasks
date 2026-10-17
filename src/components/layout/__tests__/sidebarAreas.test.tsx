/**
 * Putting an area's projects away.
 *
 * An area is permanent, so what is filed under it accumulates — the same
 * property that made Tags collapsible, arriving one level deeper. What is
 * pinned here is mostly the ways a collapse can hide something you still need:
 * the row you are standing in, the field you are typing into, and the count
 * that is supposed to describe everything underneath.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

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

/** CXVentures holds two projects; Music holds none. */
const RECORDS = [
  node("cxventures", { kind: "area", open_count: 12 }),
  node("gov certs", { kind: "project", parent: "cxventures", open_count: 4 }),
  node("lsa launch", { kind: "project", parent: "cxventures", open_count: 3 }),
  node("music", { kind: "area", open_count: 3 }),
];

const setProject = vi.fn(async () => {});

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
    setProject,
  } as never);
  useUiStore.setState({ areasCollapsed: false, collapsedAreas: [] });
});

/** The project rows on screen, by name. */
function rows(): string[] {
  return RECORDS.map((p) => p.name).filter(
    (n) => screen.queryByRole("button", { name: new RegExp(`^${n}`) }) !== null
  );
}

const chevron = (area: string) =>
  screen.getByRole("button", { name: new RegExp(`projects in ${area}$`) });

describe("collapsing an area", () => {
  it("collapses the whole Areas section and remembers the choice", () => {
    render(<Sidebar />);
    const header = screen.getByRole("button", { name: /^Areas/ });

    fireEvent.click(header);

    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(rows()).toEqual([]);
    expect(localStorage.getItem("cxtasks.areasCollapsed")).toBe("1");

    fireEvent.click(screen.getByRole("button", { name: "New area" }));
    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByPlaceholderText("Area name…")).toBeInTheDocument();
  });

  it("only offers the control to an area that holds projects", () => {
    // A disclosure over nothing is a control that takes a click and changes
    // nothing, which is what teaches you to distrust the ones that work.
    render(<Sidebar />);

    expect(chevron("cxventures")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /projects in music$/ })
    ).not.toBeInTheDocument();
  });

  it("hides the projects underneath it, and nothing else", () => {
    render(<Sidebar />);
    expect(rows()).toEqual(["cxventures", "gov certs", "lsa launch", "music"]);

    fireEvent.click(chevron("cxventures"));

    // The chevron is a control IN the row, not the row: putting an area away
    // must not also navigate into it.
    expect(setProject).not.toHaveBeenCalled();
    expect(rows()).toEqual(["cxventures", "music"]);
    // The area's own count rolls its children up server-side, so putting them
    // away must not change what it reports — a collapsed area still says how
    // much is under it.
    expect(
      screen.getByRole("button", { name: /^cxventures/ }).textContent
    ).toContain("12");
  });

  it("is remembered across a restart", () => {
    render(<Sidebar />);
    fireEvent.click(chevron("cxventures"));

    // A fresh store, as a relaunch builds one. The preference has to survive in
    // storage, not just in the module that wrote it.
    expect(
      JSON.parse(localStorage.getItem("cxtasks.collapsedAreas") ?? "[]")
    ).toEqual(["p-cxventures"]);
  });

  it("keys the memory on the id, so a rename doesn't spring it open", () => {
    // Every text join moves in a rename (invariant 17). This is not a join —
    // it is a note about a row — so it rides the id instead.
    useUiStore.setState({ collapsedAreas: ["p-cxventures"] });
    useTaskStore.setState({
      projectRecords: RECORDS.map((p) => ({
        ...p,
        // Parenthood is keyed by `parent_id`, so the children report the new
        // name the moment the area has one — this is what the sidebar sees
        // after a rename, not a hand-built variant of it.
        ...(p.id === "p-cxventures" ? { name: "cx ventures" } : {}),
        ...(p.parent === "cxventures" ? { parent: "cx ventures" } : {}),
      })),
    } as never);

    render(<Sidebar />);

    expect(
      screen.getByRole("button", { name: /projects in cx ventures$/ })
    ).toHaveAttribute("aria-expanded", "false");
  });

  it("still draws the project you are standing in", () => {
    // The sidebar's job is to answer "where am I". Hiding the one highlighted
    // row to save four pixels leaves the whole nav with nothing selected.
    useTaskStore.setState({ activeProject: "lsa launch" } as never);
    useUiStore.setState({ collapsedAreas: ["p-cxventures"] });

    render(<Sidebar />);

    expect(rows()).toEqual(["cxventures", "lsa launch", "music"]);
    expect(screen.getByRole("button", { name: /^lsa launch/ })).toHaveAttribute(
      "aria-current",
      "page"
    );
  });

  it("leaves the row itself clickable", () => {
    // The chevron is a second control in the row, not a replacement for it.
    render(<Sidebar />);

    fireEvent.click(screen.getByRole("button", { name: /^cxventures/ }));

    expect(setProject).toHaveBeenCalledWith("cxventures");
    expect(rows()).toContain("gov certs");
  });
});

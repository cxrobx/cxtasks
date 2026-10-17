/**
 * Icon & Colour: what a row wears, and what the picker saves.
 *
 * The rendering half pins the defaults Chris set by hand on 2026-09-27 — an
 * area with nothing chosen stays ink, a project's target takes an automatic
 * colour — and that a CHOSEN icon survives the chevron that replaces an area's
 * folder, which is exactly the areas (CXVentures, Products) it was chosen for.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Folder } from "lucide-react";

import { NodeIcon } from "@/lib/nodeIcons";
import { IconPicker } from "@/components/layout/IconPicker";
import { Sidebar } from "@/components/layout/Sidebar";
import { useTaskStore } from "@/stores/taskStore";
import { useUiStore } from "@/stores/uiStore";
import type { Project } from "@/types/task";

const repoCandidates = vi.fn(async () => [
  { label: "cxmail/src-tauri/icons/icon.png", dataUrl: "data:image/png;base64,QUFB", cutout: false },
]);

vi.mock("@/lib/tauri", () => ({
  api: {
    icons: { repoCandidates: (...a: unknown[]) => repoCandidates(...(a as [])), fromFile: vi.fn() },
    tags: { usage: vi.fn(async () => 0) },
    budget: { status: vi.fn(async () => null) },
    runner: { isInstalled: vi.fn(async () => false) },
    runs: { recent: vi.fn(async () => []) },
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn(async () => false), open: vi.fn() }));

const PNG = "data:image/png;base64,QUFB";

describe("NodeIcon", () => {
  it("draws an app icon as an image, untouched by colour", () => {
    const { container } = render(<NodeIcon icon={PNG} color="red" fallback={Folder} />);
    const img = container.querySelector("img");
    expect(img?.getAttribute("src")).toBe(PNG);
    expect(container.querySelector("svg")).toBeNull();
  });

  it("colours a built-in glyph by the stored hue, then the automatic one, else ink", () => {
    const stored = render(<NodeIcon icon="lucide:rocket" color="teal" fallback={Folder} autoHue="red" />);
    expect(stored.container.querySelector("svg")?.getAttribute("style")).toContain("--hue-teal");

    const auto = render(<NodeIcon icon={null} color={null} fallback={Folder} autoHue="red" />);
    expect(auto.container.querySelector("svg")?.getAttribute("style")).toContain("--hue-red");

    const ink = render(<NodeIcon icon={null} color={null} fallback={Folder} />);
    expect(ink.container.querySelector("svg")?.getAttribute("class")).toContain("text-content-muted");
  });

  it("falls back to the row's own glyph for a name this build does not know", () => {
    // A newer build's icon, synced over, must not render as nothing.
    const { container } = render(<NodeIcon icon="lucide:not-a-thing" color={null} fallback={Folder} />);
    expect(container.querySelector("svg")?.getAttribute("class")).toContain("lucide-folder");
  });

  it("puts a closed project back to ink and fades its image", () => {
    const glyph = render(<NodeIcon icon={null} color="blue" fallback={Folder} dim />);
    expect(glyph.container.querySelector("svg")?.getAttribute("style")).toBeNull();
    const img = render(<NodeIcon icon={PNG} color={null} fallback={Folder} dim />);
    expect(img.container.querySelector("img")?.getAttribute("class")).toContain("opacity-50");
  });
});

function node(name: string, overrides: Partial<Project> & { kind: Project["kind"] }): Project {
  return {
    id: `p-${name}`,
    name,
    color: null,
    icon: null,
    sort_order: 0,
    archived: false,
    open_count: 1,
    health: null,
    parent: null,
    target_date: null,
    closed_at: null,
    ...overrides,
  };
}

describe("the sidebar row", () => {
  beforeEach(() => {
    localStorage.clear();
    useUiStore.setState({ areasCollapsed: false, collapsedAreas: [] });
  });

  it("keeps a chosen icon beside the chevron of an area that holds projects", () => {
    useTaskStore.setState({
      projectRecords: [
        node("CXVentures", { kind: "area", icon: PNG }),
        node("Gov Contracting", { kind: "project", parent: "CXVentures" }),
        node("Chris X", { kind: "area" }),
      ],
      activeProject: null,
      allTags: [],
      activeTags: [],
      view: "today",
      query: "",
      filters: [],
    } as never);
    render(<Sidebar />);
    const cx = screen.getByRole("button", { name: /^CXVentures/ });
    expect(cx.querySelector("img")?.getAttribute("src")).toBe(PNG);
    // An area with nothing chosen and no projects keeps its plain ink folder.
    const chris = screen.getByRole("button", { name: /^Chris X/ });
    expect(chris.querySelector("img")).toBeNull();
    expect(chris.querySelector("svg")?.getAttribute("style")).toBeNull();
  });
});

describe("IconPicker", () => {
  const setAppearance = vi.fn(async () => null);
  beforeEach(() => {
    vi.clearAllMocks();
    useTaskStore.setState({ setAppearance } as never);
  });

  it("offers the repo icons and saves the one clicked, keeping the colour", async () => {
    render(
      <IconPicker target={{ kind: "project", name: "CXMail", icon: null, color: "blue" }} onClose={() => {}} />
    );
    const tile = await screen.findByRole("button", { name: "cxmail/src-tauri/icons/icon.png" });
    fireEvent.click(tile);
    await waitFor(() => expect(setAppearance).toHaveBeenCalledWith("project", "CXMail", "blue", PNG));
  });

  it("saves a built-in glyph and a colour, and Reset clears both", async () => {
    render(
      <IconPicker target={{ kind: "tag", name: "legal", icon: null, color: null }} onClose={() => {}} />
    );
    expect(repoCandidates).not.toHaveBeenCalled(); // a tag has no repos to look in
    fireEvent.click(screen.getByRole("button", { name: "scale" }));
    await waitFor(() => expect(setAppearance).toHaveBeenLastCalledWith("tag", "legal", null, "lucide:scale"));
    fireEvent.click(screen.getByRole("button", { name: "purple" }));
    await waitFor(() =>
      expect(setAppearance).toHaveBeenLastCalledWith("tag", "legal", "purple", "lucide:scale")
    );
    fireEvent.click(screen.getByRole("button", { name: "Reset to default" }));
    await waitFor(() => expect(setAppearance).toHaveBeenLastCalledWith("tag", "legal", null, null));
  });
});

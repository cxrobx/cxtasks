/**
 * The hosts pane, and specifically the sentence each row uses to explain
 * itself.
 *
 * The question this pane answers ninety times in a hundred is "why has nothing
 * run", and the three answers — switched off, asleep, capped too low — are
 * different problems with different fixes. A row that says "idle" for all three
 * is a dead end, which is the failure these tests exist to catch.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { HostSettings } from "@/components/layout/HostSettings";
import type { HostEntry, HostPolicy } from "@/lib/tauri";

const list = vi.fn<() => Promise<HostEntry[]>>();
const setPolicy = vi.fn<(id: string, p: HostPolicy) => Promise<void>>();
const remove = vi.fn<(id: string) => Promise<boolean>>();

vi.mock("@/lib/tauri", () => ({
  api: {
    hosts: {
      list: () => list(),
      setPolicy: (id: string, p: HostPolicy) => setPolicy(id, p),
      setMirrorRemote: vi.fn(),
      remove: (id: string) => remove(id),
    },
  },
}));

const host = (over: Partial<HostEntry["capability"]> = {}, entry: Partial<HostEntry> = {}): HostEntry => ({
  capability: {
    id: "nas",
    label: "the NAS",
    max_tier: "read-only",
    executors: ["claude"],
    workspaces: ["mirror"],
    can_checkpoint: false,
    max_concurrent: 1,
    headroom_pct: 50,
    priority: 100,
    enabled: true,
    last_seen_at: "2026-08-21 20:00:00",
    paused: false,
    gate: "Open",
    executors_ready: ["claude"],
    ...over,
  },
  mirrors: [],
  mirror_remote: null,
  is_this_host: false,
  alive: true,
  ...entry,
});

beforeEach(() => {
  vi.clearAllMocks();
  setPolicy.mockResolvedValue(undefined);
  remove.mockResolvedValue(true);
});

describe("HostSettings", () => {
  it("says a machine appears by itself when nothing is registered", async () => {
    list.mockResolvedValue([]);
    render(<HostSettings />);
    // Not "no hosts" — the useful half is that there is nothing to add by hand.
    expect(await screen.findByText(/nothing to add by hand/)).toBeTruthy();
  });

  it("names the capability rather than only the machine", async () => {
    list.mockResolvedValue([host()]);
    render(<HostSettings />);
    expect(await screen.findByText("the NAS")).toBeTruthy();
    expect(screen.getByText(/read-only · mirror/)).toBeTruthy();
    expect(screen.getByText(/1 at a time/)).toBeTruthy();
    expect(screen.getByText(/leaves 50% of the budget/)).toBeTruthy();
  });

  /** The three idle reasons are different problems with different fixes, and a
   *  row that called all of them "idle" would send the reader nowhere. */
  it("distinguishes switched off from paused from asleep", async () => {
    list.mockResolvedValue([host({ enabled: false })]);
    const { unmount } = render(<HostSettings />);
    expect(await screen.findByText("switched off")).toBeTruthy();
    unmount();

    list.mockResolvedValue([host({ paused: true })]);
    const second = render(<HostSettings />);
    expect(await screen.findByText("paused")).toBeTruthy();
    second.unmount();

    list.mockResolvedValue([host({}, { alive: false })]);
    render(<HostSettings />);
    expect(await screen.findByText("not reporting in")).toBeTruthy();
  });

  /** "Has not checked in yet" and "no agent installed" look identical on the
   *  row — both are an empty ready-list — and mean opposite things: one is a
   *  machine that has not looked, the other one that looked and found nothing. */
  it("separates a host that never looked from one that looked and found nothing", async () => {
    list.mockResolvedValue([host({ executors_ready: [], last_seen_at: null })]);
    const { unmount } = render(<HostSettings />);
    expect(await screen.findByText("has not checked in yet")).toBeTruthy();
    unmount();

    list.mockResolvedValue([host({ executors_ready: [] })]);
    render(<HostSettings />);
    expect(await screen.findByText("no agent installed")).toBeTruthy();
  });

  /** The most common reason a remote host is refused, and invisible otherwise. */
  it("says when a remote host has no mirrors and no way to get any", async () => {
    list.mockResolvedValue([host()]);
    render(<HostSettings />);
    const line = await screen.findByText(/no repos mirrored here yet/);
    expect(line.textContent).toMatch(/no mirror destination set/);
  });

  it("does not mention mirrors for the machine holding the real trees", async () => {
    list.mockResolvedValue([
      host({ workspaces: ["live"], max_tier: "write" }, { is_this_host: true }),
    ]);
    render(<HostSettings />);
    expect(await screen.findByText("this Mac")).toBeTruthy();
    expect(screen.queryByText(/mirrored/)).toBeNull();
  });

  it("changes only the declared half when a ceiling is lowered", async () => {
    list.mockResolvedValue([host({ max_tier: "write" })]);
    render(<HostSettings />);
    fireEvent.change(await screen.findByRole("combobox"), { target: { value: "read-only" } });
    await waitFor(() => expect(setPolicy).toHaveBeenCalledWith("nas", { max_tier: "read-only" }));
  });

  /** Removing the machine you are sitting at would take the local runner out of
   *  its own registry — and it would put itself back on the next tick. A button
   *  that appears to work and silently undoes itself is worse than none. */
  it("offers no way to forget the machine you are using", async () => {
    list.mockResolvedValue([host({}, { is_this_host: true })]);
    render(<HostSettings />);
    await screen.findByText("the NAS");
    expect(screen.queryByRole("button", { name: /Forget/ })).toBeNull();
  });

  it("offers Forget for any other machine", async () => {
    list.mockResolvedValue([host()]);
    render(<HostSettings />);
    fireEvent.click(await screen.findByRole("button", { name: /Forget/ }));
    await waitFor(() => expect(remove).toHaveBeenCalledWith("nas"));
  });
});

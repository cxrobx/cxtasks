/**
 * Run on: the machine a task's background runs and tripwire use. Its job is to
 * make the destination legible, so these pin what it says as much as what it
 * writes.
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { RunOnField } from "@/components/tasks/RunOnField";
import { useTaskStore } from "@/stores/taskStore";
import type { Task } from "@/types/task";

const list = vi.hoisted(() => vi.fn());
vi.mock("@/lib/tauri", () => ({ api: { hosts: { list } } }));
// Radix's menu needs layout jsdom doesn't have, and it isn't what's under test.
vi.mock("@/components/shared/Select", () => ({
  Select: ({ value, options, onValueChange, ...rest }: {
    value: string;
    options: { value: string; label: string }[];
    onValueChange: (value: string) => void;
  }) => (
    <select {...rest} value={value} onChange={(e) => onValueChange(e.target.value)}>
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  ),
}));

const host = (id: string, label: string, over: Record<string, unknown> = {}, alive = true) => ({
  capability: {
    id,
    label,
    max_tier: "write",
    takes_unassigned: false,
    ...over,
  },
  mirrors: [],
  mirror_remote: null,
  is_this_host: false,
  alive,
});

const task = (over: Partial<Task> = {}) => ({ id: "t1", run_on: null, ...over }) as Task;
const fleet = () => [
  host("mini", "Mac mini", { takes_unassigned: true }),
  host("laptop", "MacBook Pro"),
  host("nas", "the NAS", { max_tier: "read-only" }),
];

describe("RunOnField", () => {
  const patch = vi.fn();
  beforeEach(() => {
    patch.mockReset();
    useTaskStore.setState({ patch } as never);
  });

  it("names the machine behind Default", async () => {
    list.mockResolvedValue(fleet());
    render(<RunOnField task={task()} />);
    expect(await screen.findByText("Default (Mac mini)")).toBeInTheDocument();
  });

  it("says nothing on a single machine, where there is no choice", async () => {
    list.mockResolvedValue([host("mini", "Mac mini", { takes_unassigned: true })]);
    const { container } = render(<RunOnField task={task()} />);
    await waitFor(() => expect(list).toHaveBeenCalled());
    await Promise.resolve();
    expect(container.querySelector('[data-testid="run-on"]')).toBeNull();
  });

  it("writes run_on and nothing else, and null for Default", async () => {
    list.mockResolvedValue(fleet());
    render(<RunOnField task={task({ run_on: "laptop" })} />);
    const trigger = await screen.findByRole("combobox", { name: "Run on" });
    fireEvent.change(trigger, { target: { value: "__default__" } });
    expect(patch).toHaveBeenCalledWith("t1", { run_on: null });
  });

  it("says a task assigned to a machine that is off will wait", async () => {
    list.mockResolvedValue([
      host("mini", "Mac mini", { takes_unassigned: true }, false),
      host("laptop", "MacBook Pro"),
    ]);
    render(<RunOnField task={task({ run_on: "mini" })} />);
    expect(await screen.findByText(/Mac mini isn't reporting in. This waits/)).toBeInTheDocument();
  });

  it("warns before a read-only machine is handed work that writes", async () => {
    list.mockResolvedValue(fleet());
    render(<RunOnField task={task({ run_on: "nas" })} tier="write" />);
    expect(await screen.findByText(/the NAS is read-only/)).toBeInTheDocument();
  });

  it("names a machine that has been removed", async () => {
    list.mockResolvedValue(fleet());
    render(<RunOnField task={task({ run_on: "gone" })} />);
    expect(await screen.findByText(/no longer exists/)).toBeInTheDocument();
  });
});

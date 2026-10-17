/**
 * The sync panel, and specifically the words it uses.
 *
 * Three states have to read differently, and getting them wrong is not a
 * cosmetic bug: "Not paired" is a setup prompt, "Paused" is a decision the user
 * made and can undo, and showing one where the other belongs either invites
 * someone to re-pair a Mac that is already paired, or tells them nothing is
 * configured when a node is sitting there remembered.
 *
 * The unpair refusal is here for a sharper reason. What queues in the outbox is
 * DELETIONS — the only thing that cannot travel as post-images — so unpairing
 * with work outstanding means a task destroyed here comes back the next time
 * the two databases meet, and nobody would connect that resurrection to having
 * unpaired weeks earlier.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { SyncSettings } from "@/components/layout/SyncSettings";
import type { ReconcileReport, SyncStatus } from "@/lib/tauri";

const status = vi.fn<() => Promise<SyncStatus>>();
const pair = vi.fn<(u: string, t: string) => Promise<string>>();
const now = vi.fn<() => Promise<ReconcileReport>>();
const setEnabled = vi.fn<(e: boolean) => Promise<void>>();
const unpair = vi.fn<(f: boolean) => Promise<void>>();

vi.mock("@/lib/tauri", () => ({
  api: {
    sync: {
      status: () => status(),
      pair: (u: string, t: string) => pair(u, t),
      now: () => now(),
      setEnabled: (e: boolean) => setEnabled(e),
      unpair: (f: boolean) => unpair(f),
    },
  },
}));

const base: SyncStatus = {
  enabled: false,
  paired: false,
  peer_id: null,
  peer_url: null,
  last_pull_at: null,
  pending: 0,
  lease_remaining: null,
  next_short_id: 94,
  host: "this-mac",
  last_attempt_at: null,
  last_error: null,
  auto_interval_seconds: 300,
};

beforeEach(() => {
  vi.clearAllMocks();
  status.mockResolvedValue(base);
  setEnabled.mockResolvedValue(undefined);
  unpair.mockResolvedValue(undefined);
});

describe("SyncSettings", () => {
  it("offers pairing when no node has ever been configured", async () => {
    render(<SyncSettings />);
    expect(await screen.findByText("Not paired")).toBeTruthy();
    expect(screen.getByPlaceholderText(/ts\.net/)).toBeTruthy();
    // The full token, not the board one — this end seeds and reserves ranges.
    expect(screen.getByText(/full/)).toBeTruthy();
  });

  it("says PAUSED rather than 'not paired' when a node is remembered", async () => {
    status.mockResolvedValue({
      ...base,
      paired: true,
      enabled: false,
      peer_id: "the-nas",
      peer_url: "http://cx:18870",
      lease_remaining: 480,
    });
    render(<SyncSettings />);

    expect(await screen.findByText("Paused")).toBeTruthy();
    expect(screen.queryByText("Not paired")).toBeNull();
    // And it offers the way back, not a fresh pairing form.
    expect(screen.getByRole("button", { name: /Resume/ })).toBeTruthy();
    expect(screen.queryByPlaceholderText(/ts\.net/)).toBeNull();
  });

  it("names the node it is syncing with, and how much lease is left", async () => {
    status.mockResolvedValue({
      ...base,
      paired: true,
      enabled: true,
      peer_id: "the-nas",
      peer_url: "http://cx:18870",
      lease_remaining: 480,
    });
    render(<SyncSettings />);
    expect(await screen.findByText(/Syncing with the-nas/)).toBeTruthy();
    expect(screen.getByText(/480 T-numbers reserved/)).toBeTruthy();
  });

  it("surfaces queued deletions, because they are what unpairing would lose", async () => {
    status.mockResolvedValue({ ...base, paired: true, enabled: true, peer_id: "n", pending: 3 });
    render(<SyncSettings />);
    expect(await screen.findByText("3 waiting to send")).toBeTruthy();
  });

  it("shows the backend's refusal verbatim when unpairing would lose work", async () => {
    status.mockResolvedValue({ ...base, paired: true, enabled: true, peer_id: "n", pending: 2 });
    unpair.mockRejectedValue(
      "2 deletion(s) have not reached the node yet. Sync once first — unpairing now means anything deleted here comes back."
    );
    render(<SyncSettings />);

    fireEvent.click(await screen.findByRole("button", { name: /Unpair/ }));

    // Verbatim, because the reason IS the instruction. A generic "couldn't
    // unpair" would leave the user with no idea what to do next.
    expect(await screen.findByText(/have not reached the node yet/)).toBeTruthy();
    expect(unpair).toHaveBeenCalledWith(false);
  });

  it("pausing keeps everything, and says so", async () => {
    status.mockResolvedValue({ ...base, paired: true, enabled: true, peer_id: "the-nas" });
    render(<SyncSettings />);

    fireEvent.click(await screen.findByRole("button", { name: /Pause/ }));

    await waitFor(() => expect(setEnabled).toHaveBeenCalledWith(false));
    expect(await screen.findByText(/still here/)).toBeTruthy();
  });

  /** No bytes moved, in the shape the Rust side sends. */
  const quiet = { uploaded: 0, downloaded: 0, failed: [], more: false };

  it("a reconcile that moved nothing says so plainly", async () => {
    status.mockResolvedValue({ ...base, paired: true, enabled: true, peer_id: "n" });
    now.mockResolvedValue({
      swept: 0,
      pushed: 0,
      failed: [],
      pulled: { inserted_or_updated: 0, deleted: 0 },
      held_back: false,
      blobs: quiet,
      lease_renewed: null,
    });
    render(<SyncSettings />);

    fireEvent.click(await screen.findByRole("button", { name: /Sync now/ }));

    expect(await screen.findByText(/Already in step/)).toBeTruthy();
  });

  /** A held-back pull is the safe outcome, so it must not read as a failure —
   *  but it must not read as success either. */
  it("explains a held-back pull instead of reporting a clean sync", async () => {
    status.mockResolvedValue({ ...base, paired: true, enabled: true, peer_id: "n" });
    now.mockResolvedValue({
      swept: 1,
      pushed: 0,
      failed: [{ local_seq: 4, err: "refused" }],
      pulled: { inserted_or_updated: 0, deleted: 0 },
      held_back: true,
      blobs: quiet,
      lease_renewed: null,
    });
    render(<SyncSettings />);

    fireEvent.click(await screen.findByRole("button", { name: /Sync now/ }));

    expect(await screen.findByText(/nothing was pulled/)).toBeTruthy();
  });

  /** **Images are counted separately from rows**, because they are a separate
   *  channel with its own failure mode. A pass that fetched three images while
   *  no row changed must not read as "already in step", and one that could not
   *  fetch an image must not read as a clean sync — a silently missing
   *  thumbnail is the whole failure this reporting exists to surface. */
  it("counts images moved, and says when one could not be fetched", async () => {
    status.mockResolvedValue({ ...base, paired: true, enabled: true, peer_id: "n" });
    now.mockResolvedValue({
      swept: 0,
      pushed: 0,
      failed: [],
      pulled: { inserted_or_updated: 0, deleted: 0 },
      held_back: false,
      blobs: {
        uploaded: 0,
        downloaded: 3,
        failed: [{ stored_name: "a.png", err: "no bytes for a.png" }],
        more: false,
      },
      lease_renewed: null,
    });
    render(<SyncSettings />);

    fireEvent.click(await screen.findByRole("button", { name: /Sync now/ }));

    const note = await screen.findByText(/3 images/);
    expect(note.textContent).toMatch(/could not be fetched/);
    expect(note.textContent).not.toMatch(/Already in step/);
  });

  /**
   * The automatic pass, and why these three assertions are the feature.
   *
   * Sync ran manual-only until 2026-09-01, and the failure it produced had no
   * symptom: `last_pull_at` sat four days old, nothing errored, and the panel
   * said "Syncing with the-nas" the whole time. A pull time on its own cannot
   * distinguish "everything is in step" from "nobody has pressed the button
   * since Tuesday" from "every attempt is being refused" — so the panel reports
   * the ATTEMPT, and each of those three states has to read differently here.
   */
  describe("the automatic pass", () => {
    const paired = { ...base, paired: true, enabled: true, peer_id: "the-nas" };

    it("says when it last checked and how often it will check again", async () => {
      status.mockResolvedValue({
        ...paired,
        last_attempt_at: new Date(Date.now() - 4 * 60_000).toISOString(),
      });
      render(<SyncSettings />);
      expect(await screen.findByText(/Checked 4 minutes ago/)).toBeTruthy();
      expect(screen.getByText(/every 5 minutes/)).toBeTruthy();
    });

    /** **The one that would have caught the bug.** Nothing has tried, and the
     *  panel must not let that pass for "in step". */
    it("says nothing has reconciled yet when no attempt has been recorded", async () => {
      status.mockResolvedValue(paired);
      render(<SyncSettings />);
      expect(await screen.findByText(/Nothing has reconciled yet/)).toBeTruthy();
    });

    /** A loop that runs and fails every five minutes is a different problem
     *  from a loop that is not running, and the reason is the instruction — so
     *  it is shown verbatim, in the error colour. */
    it("shows a failing attempt as an error, verbatim", async () => {
      status.mockResolvedValue({
        ...paired,
        last_attempt_at: new Date(Date.now() - 90_000).toISOString(),
        last_error: "could not reach the peer: connection refused",
      });
      render(<SyncSettings />);

      const line = await screen.findByText(/connection refused/);
      expect(line.className).toMatch(/text-error/);
    });

    /** Paused is a decision, not a fault — so it explains what resuming buys
     *  rather than reporting a stalled timer. */
    it("explains that a paused node reconciles with nothing", async () => {
      status.mockResolvedValue({ ...paired, enabled: false });
      render(<SyncSettings />);
      expect(await screen.findByText(/nothing reconciles on its own/)).toBeTruthy();
    });

    /**
     * **The panel re-reads on a timer, and never reconciles by being opened.**
     *
     * Both halves matter. Without the poll, this pane shows a five-minute-old
     * answer for as long as you sit looking at it — which is the staleness the
     * whole feature exists to remove, one layer up. And syncing on open would
     * make "look at Settings" a hidden side effect, so the thing you reach for
     * to diagnose the loop would be the thing masking it.
     */
    it("re-reads its status on a timer without reconciling", async () => {
      vi.useFakeTimers();
      try {
        status.mockResolvedValue(paired);
        render(<SyncSettings />);
        await vi.waitFor(() => expect(status).toHaveBeenCalledTimes(1));

        await vi.advanceTimersByTimeAsync(15_000);
        expect(status).toHaveBeenCalledTimes(2);
        expect(now).not.toHaveBeenCalled();
      } finally {
        vi.useRealTimers();
      }
    });
  });
});

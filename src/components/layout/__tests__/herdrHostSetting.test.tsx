import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { HerdrHostSetting } from "@/components/layout/HerdrHostSetting";

const herdrHost = vi.fn<() => Promise<string | null>>();
const saveHerdrHost = vi.fn<(host: string) => Promise<string | null>>();

vi.mock("@/lib/tauri", () => ({
  api: {
    dispatch: {
      herdrHost: () => herdrHost(),
      saveHerdrHost: (host: string) => saveHerdrHost(host),
    },
  },
}));

const field = () => screen.getByLabelText("Herdr host") as HTMLInputElement;
const save = () => fireEvent.click(screen.getByRole("button", { name: "Save Herdr host" }));

describe("HerdrHostSetting", () => {
  beforeEach(() => {
    herdrHost.mockReset();
    saveHerdrHost.mockReset();
  });

  it("shows the saved host", async () => {
    herdrHost.mockResolvedValue("dana@build-host");
    render(<HerdrHostSetting />);
    await waitFor(() => expect(field().value).toBe("dana@build-host"));
  });

  it("starts blank when this Mac runs its own sessions", async () => {
    herdrHost.mockResolvedValue(null);
    render(<HerdrHostSetting />);
    await waitFor(() => expect(herdrHost).toHaveBeenCalled());
    expect(field().value).toBe("");
    expect(screen.getByText(/Leave blank for this Mac/)).toBeTruthy();
  });

  it("shows what was stored, not what was typed", async () => {
    herdrHost.mockResolvedValue(null);
    saveHerdrHost.mockResolvedValue("dana@build-host");
    render(<HerdrHostSetting />);
    fireEvent.change(field(), { target: { value: "  dana@build-host  " } });
    save();
    await waitFor(() => expect(field().value).toBe("dana@build-host"));
    expect(saveHerdrHost).toHaveBeenCalledWith("  dana@build-host  ");
    expect(screen.getByText("Saved on this Mac.")).toBeTruthy();
  });

  it("says plainly when clearing it returns to this Mac", async () => {
    herdrHost.mockResolvedValue("dana@build-host");
    saveHerdrHost.mockResolvedValue(null);
    render(<HerdrHostSetting />);
    await waitFor(() => expect(field().value).toBe("dana@build-host"));
    fireEvent.change(field(), { target: { value: "" } });
    save();
    await screen.findByText("Using this Mac's Herdr.");
  });

  it("keeps the draft and shows the refusal", async () => {
    herdrHost.mockResolvedValue(null);
    saveHerdrHost.mockRejectedValue("Herdr host must be an SSH target: a name or user@host.");
    render(<HerdrHostSetting />);
    fireEvent.change(field(), { target: { value: "two words" } });
    save();
    await screen.findByText(/must be an SSH target/);
    expect(field().value).toBe("two words");
  });
});

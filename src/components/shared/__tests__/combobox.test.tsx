/**
 * The suggestion field that replaced every `<datalist>`.
 *
 * The claims that matter are the ones a datalist gave for free and a custom
 * list can quietly lose: picking a row sets the value WITHOUT blurring (a blur
 * commits, so a click-to-pick would save the half-typed word first); Enter
 * picks only while a row is highlighted, and otherwise reaches the caller's
 * own commit; and typing narrows the list.
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";

import { Combobox } from "@/components/shared/Combobox";

const OPTIONS = [{ value: "CXVentures" }, { value: "Chris X" }, { value: "pocketbuddy" }];

function Harness({ onBlur, onKeyDown }: { onBlur?: () => void; onKeyDown?: (key: string) => void }) {
  const [value, setValue] = useState("");
  return (
    <>
      <Combobox
        aria-label="Project"
        value={value}
        onValueChange={setValue}
        options={OPTIONS}
        onBlur={onBlur}
        onKeyDown={(e) => onKeyDown?.(e.key)}
      />
      <output data-testid="value">{value}</output>
    </>
  );
}

describe("Combobox", () => {
  it("opens on focus with every suggestion, and typing narrows it", () => {
    render(<Harness />);
    const input = screen.getByRole("combobox", { name: "Project" });
    fireEvent.focus(input);
    expect(screen.getAllByRole("option")).toHaveLength(3);

    fireEvent.change(input, { target: { value: "ch" } });
    expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual(["Chris X"]);
  });

  it("picks on mousedown without blurring the field", () => {
    const onBlur = vi.fn();
    render(<Harness onBlur={onBlur} />);
    const input = screen.getByRole("combobox");
    fireEvent.focus(input);
    const row = screen.getByRole("option", { name: "pocketbuddy" });
    const down = fireEvent.mouseDown(row);

    expect(down).toBe(false); // default prevented → the input keeps focus
    expect(screen.getByTestId("value").textContent).toBe("pocketbuddy");
    expect(onBlur).not.toHaveBeenCalled();
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("Enter picks the highlighted row, and otherwise reaches the caller", () => {
    const onKeyDown = vi.fn();
    render(<Harness onKeyDown={onKeyDown} />);
    const input = screen.getByRole("combobox");
    fireEvent.focus(input);

    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(screen.getByTestId("value").textContent).toBe("Chris X");
    expect(onKeyDown).not.toHaveBeenCalled();

    // List closed, nothing highlighted: Enter is the caller's commit.
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onKeyDown).toHaveBeenCalledWith("Enter");
  });

  it("Escape closes the list without reaching the caller", () => {
    const onKeyDown = vi.fn();
    render(<Harness onKeyDown={onKeyDown} />);
    const input = screen.getByRole("combobox");
    fireEvent.focus(input);
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(onKeyDown).not.toHaveBeenCalled();
  });
});

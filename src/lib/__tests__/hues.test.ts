import { describe, expect, it } from "vitest";
import { HUES, hueFor } from "@/lib/hues";

describe("hueFor", () => {
  it("gives a name the same hue every time, whatever its case", () => {
    expect(hueFor("CXVentures")).toBe(hueFor("CXVentures"));
    // Names are unique case-insensitively, so they must colour that way too.
    expect(hueFor("cxventures")).toBe(hueFor("CXVentures"));
  });

  it("honours a stored hue, and ignores a stored value that is not one", () => {
    expect(hueFor("CXVentures", "pink")).toBe("pink");
    expect(hueFor("CXVentures", "#ff0000")).toBe(hueFor("CXVentures"));
    expect(hueFor("CXVentures", "")).toBe(hueFor("CXVentures"));
  });

  it("never hashes a live row to grey, which means put away", () => {
    for (let i = 0; i < 500; i += 1) expect(hueFor(`project-${i}`)).not.toBe("grey");
  });

  it("spreads names across the palette rather than clumping", () => {
    const seen = new Set(Array.from({ length: 200 }, (_, i) => hueFor(`p${i}`)));
    expect(seen.size).toBe(HUES.length - 1);
  });

  it("tells names apart that differ only in letter order", () => {
    // What a sum of char codes gets wrong: every anagram would collide.
    const pairs = [["ops", "sop"], ["team", "meat"], ["listen", "silent"]];
    expect(pairs.some(([a, b]) => hueFor(a) !== hueFor(b))).toBe(true);
  });
});

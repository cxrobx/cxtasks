import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { defaultSchema } from "rehype-sanitize";
import { Markdown } from "@/components/shared/Markdown";
import allowlist from "../../../crates/cxtasks-core/src/markdown_allowlist.json";

const openUrl = vi.fn<(href: string) => Promise<void>>();

vi.mock("@/lib/openUrl", () => ({
  openUrl: (href: string) => openUrl(href),
}));

beforeEach(() => openUrl.mockReset().mockResolvedValue(undefined));

describe("the markdown surface", () => {
  /** The bug this exists to fix: agent handoffs are written in markdown, and
   *  the pane used to print the syntax. */
  it("renders structure as elements instead of printing its syntax", () => {
    const { container } = render(
      <Markdown>{[
        "## Handoff: the 429",
        "",
        "**Scope:** read-only. See `lib/webhook.ts:88`.",
        "",
        "- first finding",
        "- second finding",
      ].join("\n")}</Markdown>
    );

    expect(screen.getByText("Handoff: the 429").tagName).toMatch(/^H\d$/);
    expect(container.querySelectorAll("li")).toHaveLength(2);
    expect(screen.getByText("Scope:").tagName).toBe("STRONG");
    expect(screen.getByText("lib/webhook.ts:88").tagName).toBe("CODE");
    // The literal markers must be gone, not merely styled over.
    expect(container.textContent).not.toContain("##");
    expect(container.textContent).not.toContain("**");
  });

  it("renders GFM tables and fenced code", () => {
    const { container } = render(
      <Markdown>{[
        "| a | b |",
        "| - | - |",
        "| 1 | 2 |",
        "",
        "```",
        "cargo test",
        "```",
      ].join("\n")}</Markdown>
    );
    expect(container.querySelector("table")).not.toBeNull();
    expect(container.querySelectorAll("td")).toHaveLength(2);
    expect(container.querySelector("pre")?.textContent).toContain("cargo test");
  });

  /** This text is written by an UNATTENDED agent against a repo's contents,
   *  so there is no human in the loop to notice markup that shouldn't be
   *  there. Inline HTML mounts since 2026-09-22 — through a sanitize
   *  allowlist (`lib/markdownSchema.ts`) — so the guard is the schema, and
   *  this test pins what it must never let through. */
  it("mounts safe inline HTML but strips anything that can run", () => {
    const { container } = render(
      <Markdown>{[
        '<img src="x" onerror="alert(1)"> and <b>bold</b>',
        '<script>alert(2)</script>',
        '<iframe src="https://evil.example"></iframe>',
        '<a href="javascript:alert(3)">go</a>',
        '<div onclick="alert(4)" style="color:red">styled</div>',
        "<style>body{display:none}</style>",
        '<object data="x"></object><embed src="x"><form action="/x"></form>',
      ].join("\n\n")}</Markdown>
    );
    // What may mount: the plain tags, with their inert attributes.
    expect(container.querySelector("b")?.textContent).toBe("bold");
    expect(container.querySelector("div[style]")?.getAttribute("style")).toContain("red");
    // What may not: anything executable, or that reaches out of the note.
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("iframe")).toBeNull();
    expect(container.querySelector("object")).toBeNull();
    expect(container.querySelector("embed")).toBeNull();
    expect(container.querySelector("form")).toBeNull();
    expect(container.querySelector("style")).toBeNull();
    expect(container.querySelector("[onerror]")).toBeNull();
    expect(container.querySelector("[onclick]")).toBeNull();
    expect(container.querySelector("a")?.getAttribute("href") ?? "").not.toMatch(/javascript/i);
    // The image survives with its handler stripped — `src="x"` is relative,
    // which the protocol list refuses, so even the src is gone.
    expect(container.querySelector("img")?.getAttribute("onerror") ?? null).toBeNull();
    expect(container.textContent).not.toContain("alert(2)");
    expect(container.textContent).not.toContain("display:none");
  });

  /** The reason HTML mounts at all: an Onyx-style diagram is inline SVG. */
  it("mounts an inline SVG diagram with its markers and styling intact", () => {
    const { container } = render(
      <Markdown>{[
        '<figure>',
        '<svg viewBox="0 0 440 90" width="440" height="90" role="img" aria-label="flow">',
        '<defs><marker id="a" markerWidth="9" markerHeight="9" refX="6" refY="3" orient="auto">',
        '<path d="M0,0 L6,3 L0,6 Z" fill="var(--clay)"/></marker></defs>',
        '<rect x="6" y="28" width="96" height="34" rx="8" fill="#FFFFFF" stroke="#D1CFC5" stroke-width="1.5"/>',
        '<text x="54" y="49" fill="#141413" font-size="12" text-anchor="middle">request</text>',
        '<line x1="104" y1="45" x2="168" y2="45" stroke="#D97757" stroke-width="2" marker-end="url(#a)"/>',
        '<foreignObject><body onload="alert(1)"></body></foreignObject>',
        "</svg>",
        "<figcaption>Token bucket</figcaption>",
        "</figure>",
      ].join("")}</Markdown>
    );
    const svg = container.querySelector("svg");
    expect(svg).not.toBeNull();
    expect(svg?.getAttribute("viewBox")).toBe("0 0 440 90");
    // The marker keeps its id UNPREFIXED, or `marker-end="url(#a)"` dangles.
    expect(container.querySelector("marker")?.getAttribute("id")).toBe("a");
    expect(container.querySelector("line")?.getAttribute("marker-end")).toBe("url(#a)");
    expect(container.querySelector("rect")?.getAttribute("stroke-width")).toBe("1.5");
    expect(container.querySelector("text")?.textContent).toBe("request");
    expect(container.querySelector("figcaption")?.textContent).toBe("Token bucket");
    // `foreignObject` re-enters HTML from inside SVG and is refused outright.
    expect(container.querySelector("foreignObject")).toBeNull();
    expect(container.querySelector("[onload]")).toBeNull();
  });

  /** The MCP's render check reads the same JSON, so it can only warn about
   *  what this pane really drops if the JSON names every attribute the
   *  sanitizer allows — including the defaults an upgrade might add. */
  it("keeps the shared allowlist a superset of the sanitizer's defaults", () => {
    const shared = allowlist.attributes as Record<string, string[]>;
    for (const [tag, entries] of Object.entries(defaultSchema.attributes ?? {})) {
      for (const e of entries) {
        const name = typeof e === "string" ? e : String(e[0]);
        expect(shared[tag] ?? [], `${tag}.${name} missing from markdown_allowlist.json`).toContain(
          name
        );
      }
    }
    for (const tag of defaultSchema.tagNames ?? []) {
      expect([...allowlist.html, ...allowlist.svg]).toContain(tag);
    }
  });

  it("drops exactly the attributes the shared list leaves out", () => {
    const { container } = render(
      <Markdown>{
        '<svg viewBox="0 0 9 9" xmlns:xlink="http://www.w3.org/1999/xlink"><line x1="0" y1="0" x2="9" y2="9" stroke-miterlimit="4" data-x="1"/></svg>\n\n<div data-y="2" aria-hidden="true" title="t">d</div>'
      }</Markdown>
    );
    const line = container.querySelector("line")!;
    expect(line.getAttribute("stroke-miterlimit")).toBe("4");
    expect(line.hasAttribute("data-x")).toBe(false);
    expect(container.querySelector("svg")!.hasAttribute("xmlns:xlink")).toBe(true);
    const div = container.querySelector('div[title="t"]');
    expect(div, "the div and its title survive").not.toBeNull();
    expect(div!.hasAttribute("data-y")).toBe(false);
    expect(div!.hasAttribute("aria-hidden")).toBe(false);
  });

  it("renders a GFM task list as read-only checkboxes", () => {
    const { container } = render(
      <Markdown>{["- [x] done", "- [ ] not yet"].join("\n")}</Markdown>
    );
    const boxes = container.querySelectorAll('input[type="checkbox"]');
    expect(boxes).toHaveLength(2);
    expect((boxes[0] as HTMLInputElement).checked).toBe(true);
    expect((boxes[1] as HTMLInputElement).checked).toBe(false);
    expect((boxes[1] as HTMLInputElement).readOnly).toBe(true);
  });

  /** A flex `li` makes every text run, `code` and `strong` in a checklist
   *  line its own column — T208's notes shredded into one-word stacks. jsdom
   *  has no layout, so the guard is on the classes that would cause it. */
  it("never lays a task-list item out as flex", () => {
    const { container } = render(
      <Markdown>{"- [ ] **bold** (`a`, `b`): text"}</Markdown>
    );
    const wrapper = container.firstElementChild as HTMLElement;
    expect(wrapper.className).not.toMatch(/\[&_li\.task-list-item\]:(inline-)?(flex|grid)\b/);
    const li = container.querySelector("li.task-list-item") as HTMLElement;
    expect(li.className).not.toMatch(/\b(inline-)?(flex|grid)\b/);
    expect(li.querySelector("code")?.parentElement).toBe(li);
  });

  /** `.cx-prose`'s `overflow-wrap: anywhere` drops a cell's min-content to one
   *  character, and auto table layout squeezed T221's label column to a letter
   *  per line. Cells wrap by word; only code keeps `anywhere`. */
  it("wraps table cells by word, and only code anywhere", () => {
    const { container } = render(
      <Markdown>{"| Piece | Where |\n|---|---|\n| Telly API client | `backend/src/telly/x.py` |"}</Markdown>
    );
    for (const cell of container.querySelectorAll("th, td")) {
      expect(cell.className).toContain("[overflow-wrap:break-word]");
      expect(cell.className).toContain("[&_code]:[overflow-wrap:anywhere]");
    }
  });

  it("treats a single newline as a space unless `breaks` is on", () => {
    const src = "line one\nline two";
    expect(render(<Markdown>{src}</Markdown>).container.querySelector("br")).toBeNull();
    expect(
      render(<Markdown breaks>{src}</Markdown>).container.querySelector("br")
    ).not.toBeNull();
  });

  it("reflows a long unbroken paragraph only when `reflow` is on", () => {
    const wall = "This is a sentence that runs on. ".repeat(20).trim();
    expect(render(<Markdown>{wall}</Markdown>).container.querySelectorAll("p")).toHaveLength(1);
    expect(
      render(<Markdown reflow>{wall}</Markdown>).container.querySelectorAll("p").length
    ).toBeGreaterThan(1);
  });

  /** A bare <a> in a WKWebView replaces the app with the page and there is no
   *  way back — recovering needs a relaunch. */
  it("hands links to the OS instead of navigating the webview", () => {
    render(<Markdown>{"see [the docs](https://example.com/x)"}</Markdown>);
    const link = screen.getByText("the docs");

    const event = new MouseEvent("click", { bubbles: true, cancelable: true });
    fireEvent(link, event);

    expect(event.defaultPrevented).toBe(true);
    expect(openUrl).toHaveBeenCalledWith("https://example.com/x");
  });
});

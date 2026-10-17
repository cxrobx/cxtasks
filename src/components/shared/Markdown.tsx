import ReactMarkdown from "react-markdown";
import remarkBreaks from "remark-breaks";
import remarkGfm from "remark-gfm";
import rehypeRaw from "rehype-raw";
import rehypeSanitize from "rehype-sanitize";
import { markdownSchema } from "@/lib/markdownSchema";
import { openUrl } from "@/lib/openUrl";
import { shapeProse } from "@/lib/prose";
import { cn } from "@/lib/utils";

/**
 * The one markdown surface in the app.
 *
 * Every place that renders authored prose — a note, a comment body, a run's
 * result — goes through this, so the three cannot drift into rendering the
 * same string differently. That matters more than it sounds: `finish_run`
 * writes a short comment and the full handoff onto the run, and those are read
 * side by side in the same pane.
 *
 * **Embedded HTML mounts, SANITIZED.** Since 2026-09-22 `rehype-raw` parses
 * inline HTML and `rehype-sanitize` runs an allowlist over it
 * (`lib/markdownSchema.ts`): inline SVG, styled `div`s, `details`, `mark`,
 * `figure` and the rest of what an Onyx-style diagram is made of come through;
 * `script`, `iframe`, `<style>` blocks, every `on*` handler and every
 * `javascript:` URL do not. Before this the raw pass was absent and all HTML
 * was dropped, which was the right default for text an *unattended* agent
 * writes against a repo's contents — and it is still the threat: the schema is
 * the guard now, so widen it by adding to the list, never by removing the
 * sanitize step. The tests pin both halves.
 *
 * `reflow` and `breaks` are for Notes, where the author is as often a person
 * as an agent: `reflow` breaks a long unstructured paragraph into readable
 * ones at render time (`lib/prose.ts`, the stored text is untouched), and
 * `breaks` makes a single Return a line break rather than a space, which is
 * what someone typing a note expects. Neither is on for run results, which are
 * agent markdown with its own paragraphing.
 *
 * Inline `code` gets chip styling, and the `[&_pre_code]` reset below strips
 * that back off inside a fenced block. That reset is why fences work whether or
 * not they carry a language: a bare ``` produces a `code` with no className, so
 * className-sniffing to tell inline from block silently mis-styles exactly the
 * fences agents write most.
 */
/**
 * How a table cell wraps. `.cx-prose` sets `overflow-wrap: anywhere`, which
 * keeps a long path from widening the pane — but `anywhere` also drops every
 * cell's MIN-CONTENT width to one character, and auto table layout believes
 * it: a narrow column (usually the first, short labels) was squeezed to a
 * letter per line while its neighbours took the room. So words in a cell wrap
 * as `break-word` (whole words count toward the column's minimum) and only
 * `code`, where the long unbreakable tokens live, keeps `anywhere`. The
 * table's minimum is then its longest prose words, which fits any pane.
 */
const CELL_WRAP = "[overflow-wrap:break-word] [&_code]:[overflow-wrap:anywhere]";

export function Markdown({
  children,
  className,
  reflow = false,
  breaks = false,
}: {
  children: string;
  className?: string;
  /** Split long unstructured paragraphs at sentence boundaries (render only). */
  reflow?: boolean;
  /** Render a single newline as a line break. */
  breaks?: boolean;
}) {
  const source = reflow ? shapeProse(children) : children;
  return (
    <div
      className={cn(
        // `cx-prose` maps the HTML artifact kit's colour tokens onto the app
        // theme (globals.css), so a diagram pasted from an Onyx page paints in
        // the pane's own palette.
        "cx-prose leading-[1.65] text-content-secondary",
        "[&>*:first-child]:mt-0 [&>*:last-child]:mb-0",
        // Fenced blocks scroll on their own axis. Without this a long command
        // widens the whole detail pane instead of scrolling inside its block.
        "[&_pre]:overflow-x-auto [&_pre]:rounded-md [&_pre]:bg-content/6 [&_pre]:px-2.5 [&_pre]:py-2",
        "[&_pre]:my-2 [&_pre]:font-mono [&_pre]:text-[10.5px] [&_pre]:leading-relaxed",
        "[&_pre_code]:bg-transparent [&_pre_code]:p-0 [&_pre_code]:text-inherit",
        // Embedded HTML and SVG. Diagrams are authored at a fixed width for a
        // 980px page; the pane is a third of that, so they must shrink.
        "[&_svg]:h-auto [&_svg]:max-w-full [&_img]:my-2 [&_img]:max-w-full [&_img]:rounded-md",
        "[&_figure]:my-2 [&_figure]:mx-0 [&_figcaption]:mt-1 [&_figcaption]:text-[10.5px] [&_figcaption]:text-content-muted",
        "[&_details]:my-2 [&_details]:rounded-md [&_details]:border [&_details]:border-border-subtle [&_details]:bg-content/3 [&_details]:px-2.5 [&_details]:py-1.5",
        "[&_summary]:cursor-pointer [&_summary]:font-medium [&_summary]:text-content [&_details[open]>summary]:mb-1",
        "[&_mark]:rounded [&_mark]:bg-warning/25 [&_mark]:px-0.5 [&_mark]:text-content",
        "[&_kbd]:rounded [&_kbd]:border [&_kbd]:border-border [&_kbd]:bg-content/6 [&_kbd]:px-1 [&_kbd]:font-mono [&_kbd]:text-[10.5px]",
        "[&_del]:text-content-muted [&_sub]:text-[9px] [&_sup]:text-[9px]",
        // A GFM task list: the checkbox replaces the bullet, as a HANGING
        // INDENT — the box is an inline element pulled left by its own width,
        // so wrapped lines align under the text. Never `flex` on the `li`:
        // every text run, `code` and `strong` in the line becomes its own flex
        // column, and a line with two code chips shreds into one-word stacks.
        // The negative margin also works for a loose list, where the box sits
        // inside the item's first `<p>`.
        "[&_li.task-list-item]:list-none [&_li.task-list-item]:-ml-4 [&_li.task-list-item]:pl-[18px]",
        className
      )}
    >
      <ReactMarkdown
        remarkPlugins={breaks ? [remarkGfm, remarkBreaks] : [remarkGfm]}
        rehypePlugins={[rehypeRaw, [rehypeSanitize, markdownSchema]]}
        components={{
          // Four heading sizes that read as a hierarchy at pane scale: the
          // top level carries a hairline so a note with sections scans as
          // sections, not as bold lines.
          h1: ({ children }) => (
            <h4 className="mb-1.5 mt-4 border-b border-border-subtle pb-1 text-[14px] font-semibold text-content">
              {children}
            </h4>
          ),
          h2: ({ children }) => (
            <h5 className="mb-1 mt-3.5 text-[13px] font-semibold text-content">
              {children}
            </h5>
          ),
          h3: ({ children }) => (
            <h6 className="mb-1 mt-3 text-[12px] font-semibold text-content">
              {children}
            </h6>
          ),
          h4: ({ children }) => (
            <h6 className="mb-1 mt-2.5 text-[10.5px] font-semibold uppercase tracking-wider text-content-muted">
              {children}
            </h6>
          ),
          p: ({ children }) => <p className="my-2">{children}</p>,
          ul: ({ children }) => (
            <ul className="my-2 list-disc pl-4 marker:text-content-faint">
              {children}
            </ul>
          ),
          ol: ({ children }) => (
            <ol className="my-2 list-decimal pl-4 marker:text-content-faint">
              {children}
            </ol>
          ),
          li: ({ children, className }) => (
            <li className={cn("my-0.5", className)}>{children}</li>
          ),
          // Only GFM task lists produce an input here; the schema admits no
          // other kind. It is read-only on purpose — ticking a box in a render
          // would have to write the source back, and the editor is one click
          // away.
          input: ({ checked }) => (
            <input
              type="checkbox"
              checked={Boolean(checked)}
              readOnly
              tabIndex={-1}
              className="-ml-[18px] mr-0.5 size-3 align-[-1px] accent-accent"
            />
          ),
          strong: ({ children }) => (
            <strong className="font-semibold text-content">{children}</strong>
          ),
          code: ({ children }) => (
            <code className="rounded bg-content/8 px-1 py-px font-mono text-[10.5px] text-content">
              {children}
            </code>
          ),
          blockquote: ({ children }) => (
            <blockquote className="my-2 border-l-2 border-accent/50 pl-2.5 text-content-muted [&>p]:my-1">
              {children}
            </blockquote>
          ),
          hr: () => <hr className="my-3 border-border-subtle" />,
          // GFM tables can be arbitrarily wide; the wrapper scrolls so the pane
          // does not.
          table: ({ children }) => (
            <div className="my-2 overflow-x-auto rounded-md border border-border-subtle">
              <table className="w-full border-collapse text-[11px]">{children}</table>
            </div>
          ),
          thead: ({ children }) => <thead className="bg-content/4">{children}</thead>,
          th: ({ children }) => (
            <th className={cn("border-b border-border-subtle px-2 py-1 text-left font-semibold text-content", CELL_WRAP)}>
              {children}
            </th>
          ),
          td: ({ children }) => (
            <td className={cn("border-t border-border-subtle px-2 py-1 align-top", CELL_WRAP)}>
              {children}
            </td>
          ),
          // A bare <a> in a WKWebView REPLACES the app with the page, and there
          // is no back affordance — recovering needs a relaunch. Every link
          // hands off to the OS instead.
          a: ({ href, children }) => (
            <a
              href={href}
              onClick={(e) => {
                e.preventDefault();
                if (href) void openUrl(href);
              }}
              className="text-accent underline underline-offset-2 hover:text-accent-hover"
            >
              {children}
            </a>
          ),
        }}
      >
        {source}
      </ReactMarkdown>
    </div>
  );
}

#!/usr/bin/env python3
"""Alfred Script Filter for CXTasks — `t <number or words>`.

Three lanes, fused into one list:

* **T-number** — `T69`, `t69` or a bare `69`: the exact task first, then the
  prefix band (`T11` → T110…T119), the same order the app's search uses.
* **Keyword** — the app's FTS5 index (`tasks_fts`), every word required, the
  last one prefix-matched, ranked by bm25 with the app's column weights.
* **Semantic** — cosine over `nomic-embed-text` vectors from the local Ollama.
  The vectors live in a cache OUTSIDE the CXTasks database
  (`~/Library/Caches/cxtasks-alfred/`), keyed by task id and a hash of the text,
  so a task is re-embedded only when its words change.

Read-only against the live database (`mode=ro`); nothing here writes to it.
The semantic lane is optional: Ollama down, slow or missing the model means the
list is built from the other two lanes, never an error in Alfred.

Enter opens `cxtasks://task/<id>` — the app's link handler (`src-tauri/src/links.rs`).
"""

from __future__ import annotations

import hashlib
import html
import json
import math
import os
import re
import sqlite3
import sys
import time
import urllib.request
from array import array
from datetime import datetime, timezone
from pathlib import Path

DB_PATH = Path(
    os.environ.get("CXTASKS_DATA_DIR")
    or Path.home() / "Library/Application Support/com.cxtasks.app"
) / "cxtasks.db"
CACHE_DIR = Path.home() / "Library/Caches/cxtasks-alfred"
EMBED_DB = CACHE_DIR / "embeddings.sqlite"
PREVIEW_DIR = CACHE_DIR / "previews"
APP_ICON = "/Applications/CXTasks.app"

OLLAMA = os.environ.get("OLLAMA_HOST", "http://localhost:11434").rstrip("/")
EMBED_MODEL = "nomic-embed-text"
EMBED_TIMEOUT = 1.5  # seconds for one query embedding; past it, skip the lane
BACKFILL_PER_RUN = 32  # task embeddings per keystroke; Alfred reruns until done
BACKFILL_TIMEOUT = 4.0

MAX_RESULTS = 15
RRF_K = 60
# Measured on the live tasks (2026-09-25): a paraphrase of a task scores
# 0.64–0.72 against it, while unrelated tasks sit at 0.55–0.60 for almost any
# query. So a hit must clear an absolute floor AND sit near the best score —
# a fixed floor alone padded every list with the same eight noise rows.
SEMANTIC_FLOOR = 0.57
SEMANTIC_WINDOW = 0.06
SEMANTIC_TOP = 8

# Same exclusion as the app's search: trashed out, archived in.
NOT_DELETED = "deleted_at IS NULL"

STATUS_LABEL = {
    "inbox": "Inbox",
    "todo": "To do",
    "in_progress": "In progress",
    "in_review": "In review",
    "queued": "Queued",
    "done": "Done",
}
STATUS_GLYPH = {
    "inbox": "○",
    "todo": "○",
    "in_progress": "◐",
    "in_review": "◑",
    "queued": "◌",
    "done": "✓",
}


# ── database ────────────────────────────────────────────────────────────────


def open_db() -> sqlite3.Connection:
    conn = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True, timeout=2)
    conn.row_factory = sqlite3.Row
    return conn


def short_ref(query: str) -> int | None:
    """`T69`, `t69` or a bare `69` — a lone reference, nothing else."""
    m = re.fullmatch(r"[Tt]?(\d{1,6})", query.strip())
    return int(m.group(1)) if m else None


def number_lane(conn, n: int) -> list[str]:
    rows = conn.execute(
        f"""SELECT id FROM tasks
            WHERE {NOT_DELETED} AND short_id IS NOT NULL
              AND CAST(short_id AS TEXT) LIKE ?1
            ORDER BY CASE WHEN short_id = ?2 THEN 0 ELSE 1 END, short_id
            LIMIT {MAX_RESULTS}""",
        (f"{n}%", n),
    )
    return [r["id"] for r in rows]


def match_expr(query: str) -> str | None:
    """The app's `build_match_expr`: every token quoted, the last prefixed."""
    tokens = [t for t in query.split() if any(c.isalnum() for c in t)]
    if not tokens:
        return None
    parts = []
    for i, tok in enumerate(tokens):
        esc = tok.replace('"', '""')
        last = i == len(tokens) - 1
        parts.append(f'"{esc}"*' if last and re.fullmatch(r"\w+", esc) else f'"{esc}"')
    return " AND ".join(parts)


def keyword_lane(conn, query: str) -> list[str]:
    expr = match_expr(query)
    if expr:
        try:
            rows = conn.execute(
                f"""SELECT t.id FROM tasks_fts f JOIN tasks t ON t.rowid = f.rowid
                    WHERE tasks_fts MATCH ?1 AND t.{NOT_DELETED}
                    ORDER BY bm25(tasks_fts, 4.0, 1.0, 2.0) LIMIT 40""",
                (expr,),
            )
            return [r["id"] for r in rows]
        except sqlite3.Error:
            pass
    like = f"%{query}%"
    rows = conn.execute(
        f"""SELECT id FROM tasks WHERE {NOT_DELETED}
              AND (title LIKE ?1 OR body LIKE ?1 OR project LIKE ?1)
            ORDER BY datetime(updated_at) DESC LIMIT 40""",
        (like,),
    )
    return [r["id"] for r in rows]


# ── semantic lane ───────────────────────────────────────────────────────────


def embed(texts: list[str], timeout: float) -> list[list[float]] | None:
    body = json.dumps({"model": EMBED_MODEL, "input": texts, "keep_alive": "30m"}).encode()
    req = urllib.request.Request(
        f"{OLLAMA}/api/embed", data=body, headers={"Content-Type": "application/json"}
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.load(resp)["embeddings"]
    except Exception:
        return None


def normalize(v: list[float]) -> array:
    n = math.sqrt(sum(x * x for x in v)) or 1.0
    return array("f", (x / n for x in v))


def doc_text(row) -> str:
    parts = [row["title"] or ""]
    if row["project"]:
        parts.append(f"Project: {row['project']}")
    if row["body"]:
        parts.append(row["body"][:2000])
    return "search_document: " + "\n".join(parts)


def open_cache() -> sqlite3.Connection:
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    c = sqlite3.connect(EMBED_DB, timeout=2)
    c.execute(
        "CREATE TABLE IF NOT EXISTS vec (id TEXT PRIMARY KEY, digest TEXT NOT NULL, v BLOB NOT NULL)"
    )
    return c


def sync_cache(conn, cache) -> tuple[dict[str, array], int]:
    """Every live task's vector, embedding what is missing or changed.

    Embeds at most BACKFILL_PER_RUN per call so the first run never blocks a
    keystroke; returns how many are still pending so the caller can ask Alfred
    to rerun.
    """
    rows = conn.execute(f"SELECT id, title, body, project FROM tasks WHERE {NOT_DELETED}")
    wanted: dict[str, tuple[str, str]] = {}
    for r in rows:
        text = doc_text(r)
        wanted[r["id"]] = (hashlib.sha1(text.encode()).hexdigest(), text)

    have = {
        i: (d, v) for i, d, v in cache.execute("SELECT id, digest, v FROM vec")
    }
    stale = [i for i, (d, _) in wanted.items() if i not in have or have[i][0] != d]
    gone = [i for i in have if i not in wanted]
    if gone:
        cache.executemany("DELETE FROM vec WHERE id = ?", [(i,) for i in gone])

    todo = stale[:BACKFILL_PER_RUN]
    if todo:
        vecs = embed([wanted[i][1] for i in todo], BACKFILL_TIMEOUT)
        if vecs:
            for i, v in zip(todo, vecs):
                blob = normalize(v).tobytes()
                cache.execute(
                    "INSERT OR REPLACE INTO vec (id, digest, v) VALUES (?, ?, ?)",
                    (i, wanted[i][0], blob),
                )
                have[i] = (wanted[i][0], blob)
            stale = stale[len(todo):]
    cache.commit()

    out = {}
    for i, (_, blob) in have.items():
        if i in wanted:
            a = array("f")
            a.frombytes(blob)
            out[i] = a
    return out, len(stale)


def semantic_lane(conn, query: str) -> tuple[list[str], int]:
    try:
        cache = open_cache()
        vectors, pending = sync_cache(conn, cache)
    except sqlite3.Error:
        return [], 0
    if not vectors:
        return [], pending
    q = embed(["search_query: " + query], EMBED_TIMEOUT)
    if not q:
        return [], pending
    qv = normalize(q[0])
    scored = []
    for i, v in vectors.items():
        s = sum(a * b for a, b in zip(qv, v))
        if s >= SEMANTIC_FLOOR:
            scored.append((s, i))
    scored.sort(reverse=True)
    if not scored:
        return [], pending
    cutoff = scored[0][0] - SEMANTIC_WINDOW
    return [i for s, i in scored[:SEMANTIC_TOP] if s >= cutoff], pending


# ── fusion ──────────────────────────────────────────────────────────────────


def fuse(lanes: list[list[str]]) -> list[str]:
    """Reciprocal-rank fusion: a task both lanes agree on beats either alone."""
    score: dict[str, float] = {}
    for lane in lanes:
        for rank, i in enumerate(lane):
            score[i] = score.get(i, 0.0) + 1.0 / (RRF_K + rank + 1)
    return sorted(score, key=lambda i: -score[i])


# ── presentation ────────────────────────────────────────────────────────────


def load_tasks(conn, ids: list[str]) -> dict[str, sqlite3.Row]:
    if not ids:
        return {}
    marks = ",".join("?" * len(ids))
    rows = conn.execute(f"SELECT * FROM tasks WHERE id IN ({marks})", ids)
    return {r["id"]: r for r in rows}


def tags_for(conn, task_id: str) -> list[str]:
    rows = conn.execute(
        """SELECT g.name FROM task_tags tt JOIN tags g ON g.id = tt.tag_id
           WHERE tt.task_id = ? ORDER BY g.name""",
        (task_id,),
    )
    return [r["name"] for r in rows]


def parse_ts(raw: str | None) -> datetime | None:
    """Both formats the DB holds: ISO-8601 `…Z` and SQLite's `YYYY-MM-DD HH:MM:SS`."""
    if not raw:
        return None
    s = raw.strip().replace(" ", "T").rstrip("Z")
    try:
        dt = datetime.fromisoformat(s)
    except ValueError:
        return None
    return dt.replace(tzinfo=timezone.utc).astimezone()


def when(raw: str | None) -> str | None:
    dt = parse_ts(raw)
    if not dt:
        return None
    today = datetime.now().astimezone().date()
    delta = (dt.date() - today).days
    if delta == 0:
        return "today"
    if delta == 1:
        return "tomorrow"
    if delta == -1:
        return "yesterday"
    fmt = "%b %-d" if dt.year == today.year else "%b %-d %Y"
    return dt.strftime(fmt)


def one_line(text: str | None, n: int = 160) -> str:
    s = re.sub(r"<[^>]+>", " ", text or "")  # HTML/SVG in notes
    s = re.sub(r"[#*_`>|\[\]]+", " ", s)
    s = re.sub(r"\s+", " ", s).strip()
    return s if len(s) <= n else s[: n - 1].rstrip() + "…"


# Alfred cuts an overlong title in the MIDDLE and has no setting to change that,
# so the end of the title is cut here first, at a word, before Alfred sees it.
# Measured on the default theme: ~68 characters fit beside the icon and ↩.
TITLE_MAX = 64


def clip_end(text: str, n: int = TITLE_MAX) -> str:
    if len(text) <= n:
        return text
    cut = text[: n - 1]
    if " " in cut[n // 2 :]:
        cut = cut[: cut.rindex(" ")]
    return cut.rstrip(" ,;:-–—(") + "…"


def ref(t) -> str:
    return f"T{t['short_id']}" if t["short_id"] is not None else t["id"][:8]


def meta_line(t) -> str:
    bits = [f"{STATUS_GLYPH.get(t['status'], '·')} {STATUS_LABEL.get(t['status'], t['status'])}"]
    if t["priority"] is not None and t["priority"] < 3:
        bits.append(f"P{t['priority']}")
    if t["project"]:
        bits.append(t["project"])
    due = when(t["due_at"])
    if due and t["status"] != "done":
        bits.append(f"due {due}")
    defer = parse_ts(t["defer_until"])
    if defer and defer > datetime.now().astimezone():
        bits.append(f"deferred to {when(t['defer_until'])}")
    if t["blocked_reason"]:
        bits.append("on hold")
    if t["deleted_at"]:
        bits.append("in Trash")
    elif t["archived_at"]:
        bits.append("archived")
    return "  ·  ".join(bits)


def write_preview(conn, t, tags: list[str]) -> str:
    """A Quick Look page (⇧ / ⌘Y) — the whole task, notes escaped not rendered."""
    PREVIEW_DIR.mkdir(parents=True, exist_ok=True)
    esc = html.escape
    rows = [
        ("Status", STATUS_LABEL.get(t["status"], t["status"])),
        ("Priority", f"P{t['priority']}" if t["priority"] is not None else None),
        ("Project", t["project"]),
        ("Due", when(t["due_at"])),
        ("Deferred to", when(t["defer_until"])),
        ("On hold", t["blocked_reason"]),
        ("Tags", ", ".join(tags) or None),
        ("Repo", t["repo_path"] and t["repo_path"].replace(str(Path.home()), "~", 1)),
        ("Updated", when(t["updated_at"])),
        ("Completed", when(t["completed_at"])),
    ]
    blockers = conn.execute(
        """SELECT b.short_id, b.title, b.status FROM task_deps d
           JOIN tasks b ON b.id = d.blocked_by_id
           WHERE d.task_id = ? AND b.deleted_at IS NULL""",
        (t["id"],),
    ).fetchall()
    subtasks = conn.execute(
        f"SELECT short_id, title, status FROM tasks WHERE parent_id = ? AND {NOT_DELETED} ORDER BY sort_order",
        (t["id"],),
    ).fetchall()
    comments = conn.execute(
        """SELECT author, body, created_at FROM task_comments
           WHERE task_id = ? ORDER BY created_at DESC, rowid DESC LIMIT 3""",
        (t["id"],),
    ).fetchall()

    def task_list(title, items):
        if not items:
            return ""
        lis = "".join(
            f"<li>{'✓' if r['status'] == 'done' else '○'} "
            f"<b>T{r['short_id']}</b> {esc(r['title'] or '')}</li>"
            for r in items
        )
        return f"<h2>{title}</h2><ul>{lis}</ul>"

    meta = "".join(
        f"<tr><th>{k}</th><td>{esc(str(v))}</td></tr>" for k, v in rows if v
    )
    notes = (
        f"<pre>{esc(t['body'])}</pre>" if t["body"] else "<p class=dim>No notes.</p>"
    )
    prompt = f"<h2>Seed prompt</h2><pre>{esc(t['prompt'])}</pre>" if t["prompt"] else ""
    comm = ""
    if comments:
        comm = "<h2>Latest comments</h2>" + "".join(
            f"<div class=c><div class=dim>{esc(c['author'] or '')} · {esc(when(c['created_at']) or '')}</div>"
            f"<pre>{esc((c['body'] or '')[:1200])}</pre></div>"
            for c in comments
        )
    page = f"""<!doctype html><html><head><meta charset=utf-8>
<title>{esc(ref(t))} {esc(t['title'] or '')}</title><style>
:root{{--bg:#FDF6E3;--ink:#1f1f1f;--dim:#657b83;--rule:#e4dcc4}}
@media (prefers-color-scheme:dark){{:root{{--bg:#1A1A1A;--ink:#C4C5B5;--dim:#8a8a7e;--rule:#333}}}}
body{{background:var(--bg);color:var(--ink);font:13px/1.5 "JetBrains Mono",ui-monospace,Menlo,monospace;margin:24px;max-width:860px}}
h1{{font-size:18px;margin:0 0 12px}} h1 .n{{color:var(--dim)}}
h2{{font-size:13px;color:var(--dim);text-transform:uppercase;letter-spacing:.05em;margin:22px 0 6px;border-top:1px solid var(--rule);padding-top:12px}}
table{{border-collapse:collapse}} th{{text-align:left;color:var(--dim);font-weight:normal;padding:1px 16px 1px 0;vertical-align:top}}
pre{{white-space:pre-wrap;word-wrap:break-word;font:inherit;margin:0}} ul{{margin:0;padding-left:18px}}
.dim{{color:var(--dim)}} .c{{margin-bottom:10px}}
</style></head><body>
<h1><span class=n>{esc(ref(t))}</span> {esc(t['title'] or '')}</h1>
<table>{meta}</table>
<h2>Notes</h2>{notes}
{task_list('Waiting on', blockers)}{task_list('Subtasks', subtasks)}
{prompt}{comm}
</body></html>"""
    path = PREVIEW_DIR / f"{t['id']}.html"
    path.write_text(page, encoding="utf-8")
    return str(path)


def item(conn, t) -> dict:
    tags = tags_for(conn, t["id"])
    link = f"cxtasks://task/{t['id']}"
    notes = one_line(t["body"]) or "No notes"
    extras = []
    if tags:
        extras.append("tags " + ", ".join(tags))
    if t["repo_path"]:
        extras.append(t["repo_path"].replace(str(Path.home()), "~"))
    if t["tripwire"]:
        extras.append(f"tripwire {t['tripwire']}")
    return {
        "title": clip_end(f"{ref(t)}  {t['title'] or '(untitled)'}"),
        "subtitle": meta_line(t),
        "arg": link,
        "autocomplete": ref(t),
        "quicklookurl": write_preview(conn, t, tags),
        "icon": {"type": "fileicon", "path": APP_ICON},
        "text": {
            "copy": ref(t),
            "largetype": f"{ref(t)}  {t['title']}\n\n{t['body'] or 'No notes.'}",
        },
        "mods": {
            "cmd": {"subtitle": notes, "arg": link, "valid": True},
            "alt": {"subtitle": "  ·  ".join(extras) or "No tags or repo", "arg": link, "valid": True},
        },
        "variables": {"task_ref": ref(t)},
    }


def empty_query(conn) -> list[str]:
    """Nothing typed yet: what is in motion, most recently touched first."""
    rows = conn.execute(
        f"""SELECT id FROM tasks
            WHERE deleted_at IS NULL AND archived_at IS NULL AND status != 'done'
              AND parent_id IS NULL
            ORDER BY CASE status WHEN 'in_progress' THEN 0 WHEN 'in_review' THEN 1 ELSE 2 END,
                     datetime(updated_at) DESC
            LIMIT {MAX_RESULTS}"""
    )
    return [r["id"] for r in rows]


def search(query: str) -> dict:
    query = query.strip()
    if not DB_PATH.exists():
        return {"items": [{"title": "CXTasks database not found", "subtitle": str(DB_PATH), "valid": False}]}
    conn = open_db()
    pending = 0
    n = short_ref(query)
    if not query:
        ordered = empty_query(conn)
    elif n is not None:
        # A reference is not text: the number band only, exact task first.
        ordered = number_lane(conn, n)
    else:
        lanes = [keyword_lane(conn, query)]
        if len(query) >= 3:
            sem, pending = semantic_lane(conn, query)
            lanes.append(sem)
        ordered = fuse(lanes)

    tasks = load_tasks(conn, ordered)
    # Relevance order, done tasks included: sorting open work first put an
    # unrelated open task above the done one the words described. The ✓ in the
    # subtitle already says which is which.
    live = [i for i in ordered if i in tasks]
    items = [item(conn, tasks[i]) for i in live[:MAX_RESULTS]]

    if not items:
        items = [{
            "title": f"No task matches “{query}”",
            "subtitle": "Try fewer words, or a T-number",
            "valid": False,
            "icon": {"type": "fileicon", "path": APP_ICON},
        }]
    out: dict = {"items": items, "skipknowledge": True}
    if pending:
        # Embeddings still backfilling: ask Alfred to run again so the semantic
        # lane fills in without another keystroke.
        out["rerun"] = 0.3
    return out


def main() -> None:
    query = " ".join(sys.argv[1:])
    started = time.perf_counter()
    try:
        result = search(query)
    except sqlite3.OperationalError as e:
        result = {"items": [{"title": "CXTasks database is busy or unreadable", "subtitle": str(e), "valid": False}]}
    if os.environ.get("CXTASKS_ALFRED_DEBUG"):
        print(f"{(time.perf_counter() - started) * 1000:.0f} ms", file=sys.stderr)
    json.dump(result, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()

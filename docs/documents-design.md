# Documents design: a writer and viewer next to the chat

Status: 2026-11-10. Design, plus the **viewer-only MVP built in the same PR** (§7.2, "As built"). Everything else here is still design. Builds on:

- [gui-spec.md](gui-spec.md) and [brand/](brand/) (layout, tokens, underline tabs, monotone icons, no emoji).
- Workspaces: `crates/tern-core/src/workspace.rs` (cap-std scope, one frozen workspace per run), `docs/reference.md` "Workspaces", `POST /v1/uploads`.
- Tools and policy: `crates/tern-tools/src/fs.rs` (`write_policy`, `taint_safe`), `[taint] workspace_writes`, autonomy levels in `approval.rs`.
- Code-mode safety net: `crates/tern-core/src/checkpoint.rs` (shadow git, `/v1/runs/{id}/changes|undo`).
- Ledger (`ledger_repo.rs`), journal/resume, channels ([channels-design.md](channels-design.md)), routines (`schedules`), the quarantined fetcher ([quarantined-fetch-design.md](quarantined-fetch-design.md)).
- [teams-design.md](teams-design.md) (merged, PR #36): `owner_user_id`, `visibility = private | team`, one policy layer, `Viewer`-scoped repos.

Reference products: Claude artifacts and ChatGPT canvas. A panel beside the chat where the agent writes a document, the user reads it rendered, edits it, sees what changed, goes back, exports.

Vocabulary: a **document** is one thing the user would call "the report". A **revision** is one saved state of it. A **panel** is the right-hand viewer. A **writer** is anyone who creates a revision: the user, an agent, or an outside program.

## 0. TL;DR

1. **A document is a store record plus a file.** The record (id, kind, title, owner, visibility, revision log) lives in SQLite. In a normal install the content is a **real file in a workspace** (default `~/tern-workspace/documents/`), so the user owns it and every other tool can read it. Documents with no workspace (hosted cores, Telegram-only use) are **store-only**, and "Save a copy" or export gives a file. One model, two storage modes (§2.1).
2. **Every write is a revision.** Content-addressed, zstd-compressed, authored by `agent:<id>`, `user:<id>` or `external`. Diffs are computed on demand. Restore creates a new revision, never rewrites history. Git is not used for revisions; the code-mode checkpoint stays what it is (§2.3, §2.4).
3. **Markdown first.** v1 kinds: `markdown`, `text`, `code` (any single file, syntax-highlighted), `csv` (table view). `html` is a **sandboxed preview** plus source. Mermaid fences render as diagrams. Export: md and html in the alpha, pdf next, docx after. Slides are not planned (§1.2).
4. **Agent edits are patches with optimistic concurrency.** `edit_document` takes `base_rev` and find/replace pairs. A stale base is rebased when the hunks do not overlap and refused, with the current text, when they do. The agent never overwrites a user edit it has not seen (§3.2).
5. **The agent sees what is open and selected.** The app sends a small UI context (open docs, active doc, revision, selection) that rides at the tail of the next user turn, not in the cached prefix (§3.5).
6. **Writes inside the workspace do not ask**, exactly like `write_file`. `readonly` refuses. Tainted content is allowed to be shown, is flagged, and is wrapped as data when read back (§3.3).
7. **Panel, not a new window.** The panel sits beside the conversation with underline tabs for the documents tied to that conversation. A full-screen mode and a Documents library view come in the same slice or right after (§1.3).
8. **Render with what is already in `pubspec.yaml`.** `flutter_markdown_plus` for Markdown, a plain `TextField` editor with a live preview for editing, a sanitizer plus a sandboxed webview only for HTML. No rich-text editor package in v1 (§5).
9. **Smallest alpha slice** (§7.2, "alpha-6 Docs A"): D0 + D1 + the autosave half of D2. The agent writes Markdown, the panel renders it, the user edits it, every write is a revision with a simple history list. Export, diff view, comments and sharing follow.

## 1. Product shape

### 1.1 What a document is, and how it relates to everything else

| Thing | Relationship | Why |
|---|---|---|
| **Chat turn / session** | A document is *created in* a session and keeps `session_id` and `run_id` of its first revision. The conversation shows a compact **document card** ("Weekly report, 3 revisions, Open") where the tool call happened. Each revision records its run. | The transcript stays the story; the document is the artifact. The card is the link back. |
| **Agent** | `agent_id` is the creating agent. Any agent that can reach the workspace can read it. Editing by another agent is allowed (agent-to-agent hand-offs are a feature) and shows as that agent's revision. | Matches how files behave today. |
| **Channel** | A document made in a channel gets `channel_id`. The channel header gets a **Documents** tab (underline tabs, like Memory) listing them. In teams, visibility defaults to the channel's. | Several people and agents work on one thing; it should be findable by room. |
| **Routine run** | A routine may name an output document (`output = "reports/weekly.md"`). Each run writes a **new revision** of that one document; the revision records `run_id` and `schedule_id`. Without a name, each run creates a dated document (`weekly-report-2026-11-17.md`). The finished run posts a card and a notification. | "Weekly report" is a document with a history, not a pile of files. Comparing this week to last week is the diff view. |
| **Workspace** | The file lives in one workspace; the document row stores `workspace_id` and a path relative to it (§2.2). | Confinement already works; reuse it. |
| **Owner / team** | `owner_user_id`, `visibility` (§6.6). | Teams model already decided. |

A document does not belong to exactly one conversation. `documents.session_id` is its origin, and a separate `document_links (doc_id, session_id)` row is added each time a conversation opens or touches it. The panel's tabs for a conversation are its links.

### 1.2 Kinds for v1 and later

| Kind | When | View | Edit | Notes |
|---|---|---|---|---|
| `markdown` (`.md`) | **v1, first-class** | Rendered | Source editor with live preview; toolbar for bold, link, heading, list | GFM: tables, task lists, fenced code, strikethrough, footnotes later. |
| `text` (`.txt`) | v1 | Monospace | Plain editor | Falls out of the same code path. |
| `code` (any extension) | v1 | Highlighted source | Plain editor with line numbers | One file, no run button. Language from extension. |
| `csv` / `tsv` | v1 (view), later (edit) | Table (virtualised), sort and filter in the viewer only | Edit as text in v1; cell editing is later | The agent writes CSV the same way as text. |
| Mermaid | v1, **inside Markdown** as a ```` ```mermaid ```` fence | Diagram with "view source" toggle | Edit the fence | A separate `.mmd` kind is cheap to add later. See §5.4 for how to render. |
| `html` | **v1.1 (D3b)**, preview only | Sandboxed preview, source toggle | Source edit; preview refreshes | The risky one; §5.3 is the rule set. Agent-written HTML is a design or mock, not an app: no scripts, no network. |
| `svg` | Later | As image after sanitising | Source | Treat exactly like HTML. |
| `docx`, `pdf` | **Export only** | n/a | n/a | Import of an existing `.docx` or `.pdf` opens read-only text extraction (later). |
| Slides | **Not planned** | | | Export Markdown to a slide-shaped PDF later if asked. |
| Notebooks, spreadsheets with formulas | Not planned | | | The Python sandbox and `xlsx`-style skills cover compute. |

**Recommendation:** ship Markdown, text, code and csv-as-text in the alpha. Kind is chosen from the extension at creation; `create_document` takes `kind` or infers it. Unknown extension is `text`.

### 1.3 Panel, full screen, tabs

- **Panel (default).** A third column to the right of the conversation, resizable (min 360, default 45% of the content width, max 70%), toggled with ⌘⌥D and by the document card's **Open**. It reuses the same slot as the Activity panel in `side_panel.dart`: the right pane has **two modes with underline tabs at its top: Activity and Documents**. Opening a document switches to Documents; closing the last doc tab returns to Activity. No new window, no new column.
- **Tabs.** Underline tabs across the top of the panel, one per open document, each with title, a dot when unsaved or when the agent changed it while you were looking elsewhere, and a close. Overflow scrolls; ⌘1 to ⌘9 switch. A **+** menu offers "New document", "Open file…" and "Recent".
- **Full-screen viewer.** A button (and double-click on the tab) expands the document to fill the content area, hiding the conversation; Esc returns. The viewer is the same widget with a wider reading column (max 720 px prose, code unlimited). Reasons for needing it: reading a long report, comparing two revisions side by side.
- **Documents library.** A **Documents** item under the sidebar's Library group opens a centre-column list (title, kind, agent, updated, owner, visibility) with search and filter by agent, channel, kind. It is the answer to "where did that report go".
- **Closing the app.** Open tabs per conversation are restored from `ui_state` (client-side, per server); nothing in the core depends on them.

## 2. Where documents live, and versions

### 2.1 Files, store records, or both: both

| | Files only | Store only | **Both (recommended)** |
|---|---|---|---|
| User owns the bytes, other tools read them (`grep`, an editor, git) | yes | no | **yes** |
| Rename, move, delete outside tern | silently desyncs | n/a | handled by hash check (§2.2) |
| Authorship, revisions, comments, visibility | need sidecar files | native | **native (store)** |
| Hosted core with no user-visible filesystem | breaks | works | **works (store-only mode)** |
| Teams visibility and attribution | awkward | native | **native** |

Design:

- `documents.storage = 'file'`: the content is a file at `workspace_id` + `path`. The **file is the source of truth for the current text**. The store holds the record and the revision log.
- `documents.storage = 'store'`: the content lives only as the newest revision blob. Used when the run has no usable workspace, on hosted cores, and for documents a channel member created in a team core before anyone chose a folder. A **Save to workspace…** action converts it to `file` (writes the file, sets the path, keeps the history).
- Default for a new document made by an agent: `file`, in the run's workspace under `documents/`. Default for a document the user starts from **+ New** in the app: same, with a title prompt.

**Why not hide files in `~/.tern`:** the owner's note is that the user owns it. A report that is a real `.md` in a folder the user sees in Finder is the product's differentiator against hosted canvases.

### 2.2 Paths, confinement and the default workspace

- Every file operation goes through the run's `WorkspaceScope`, using the same handle-based primitives as `fs.rs` (no `std::fs`; `clippy.toml` already bans it). A document tool cannot name a path outside the workspace. The refusal is the existing `WorkspaceRefused` event, never a question.
- **Naming.** The agent supplies `title` and optionally `path`. Default path: `documents/<slug>.<ext>` under the workspace root (or the run's `subdir`). Collisions append `-2`. Slug is lowercase ASCII, `[a-z0-9-]`, max 60 chars; the title keeps the real text. A routine's `output` path is used verbatim if confined.
- **Creating the folder:** `documents/` is created on first use, not at install.
- **A conversation's workspace changes** (new folder): documents stay bound to the workspace they were created in; the panel shows the folder as a subtitle. Opening a document whose workspace was removed shows "Folder not found" with Locate and Convert to store-only (keeps history).
- **Protected paths.** The deny set (`DenySet::for_data_dir`) applies: a document can never be created inside `~/.tern`. Symlinks and hardlinks follow the existing scope rules.
- **Opening an existing file.** Three ways, all producing a document row bound to that file in place (no copy):
  1. **Open file…** in the app: file picker (`file_selector` is already a dependency). The file must be inside a workspace the user can see; if not, the dialog offers **Add this folder as a workspace** (the existing workspaces flow, `POST /v1/workspaces`) or **Import a copy** (copies into `documents/imported/`, original untouched).
  2. **Click a path** in a chat message or tool card (`documents/plan.md`): opens it in the panel.
  3. **`open_document`-style CLI:** `tern docs open <path>` (TUI and CLI) and drag-and-drop onto the panel (desktop).
- **Import** of text-like files only in v1 (`.md .txt .csv .json .html` and source files, ≤ 5 MiB). Binary formats are refused with a clear message. Import never executes anything.
- **External changes.** A file-backed document stores `content_hash` of its current revision. On open, on window focus and on a 2-second debounced file watcher (only while the doc is open in some panel), the core re-hashes; a mismatch creates a revision authored `external` ("changed outside tern"). Deletion marks the document `missing` (record and history stay; the panel offers Restore file, from the last revision, or Archive). Rename inside tern updates the path in a single transaction with `os.rename` through the scope.

### 2.3 Revisions

Every write that changes bytes creates a revision. No-op writes (identical hash) create nothing.

- **Author:** `author_kind` is `agent`, `user`, `external` (file changed by something else) or `system` (restore, import, migration). `author_id` is the agent id, user id (`owner` for legacy), or a device name for external. `run_id` and `tool_call_id` link back to the transcript.
- **Storage:** `document_revisions` holds metadata; bytes are in `document_blobs`, keyed by blake3 of the content, zstd-compressed. Identical content across revisions or documents is stored once. Full snapshots, not deltas: a 200 KB document with 100 revisions is a few MB after compression, restore and read are O(1), and there is no chain to corrupt. If size ever matters, a periodic delta thinning job is possible without changing the API.
- **Coalescing:** an agent edit is one revision per tool call (a streamed write is one revision, at the end; partial streams are never revisions). User typing is coalesced: autosave writes after 1.5 s of idle, and consecutive user saves within **5 minutes** by the same author on the same base update the **same** revision in place (`updated_at` moves, `rev` stays) until the window closes, an agent writes, or the user presses Save version. So a writing session is a handful of revisions, not hundreds.
- **Labels:** the user can name a version ("sent to Dana"); labelled revisions are never thinned.
- **Retention:** keep everything for 90 days; after that keep labelled revisions, the first and latest of each day, and every revision that is an agent write. A per-doc cap of 1000 revisions applies thinning early. Document deletion is a soft delete (trash, 30 days) then a purge of rows and orphan blobs.
- **Restore:** "Restore this version" writes a new revision with the old content, `author_kind = system`, `summary = "restored rev 4"`. History is append-only.
- **Diff view:** a unified or side-by-side diff of any two revisions, computed on demand in the core (`similar` crate for line diff, a word-level diff inside changed lines) and returned as hunks; the client renders them. Between the previous revision and the current is the default and what "what changed" highlights in the panel use (§4.2).
- **Summary:** each revision has a one-line `summary`: the agent's `note` argument if given, else generated cheaply by the core ("+12 −3 lines", "replaced 2 passages"), never an extra model call.

### 2.4 Relationship to the shadow-git checkpoint

They are different tools for different questions and **stay separate**:

| | Checkpoint (code mode) | Document revisions |
|---|---|---|
| Question | "Undo everything this run did to my folder" | "Show and restore this one document's history" |
| Unit | one commit per run, whole workspace | one revision per write, one document |
| Authorship | the run | agent, user, external |
| Needs git binary | yes | no |
| Includes user edits | only as "files differ from snapshot" | explicitly, with author |
| Retention | per run, caps at 50k files | per document, thinned by age |

Interactions, resolved:

1. Document writes in a code-mode run are ordinary file changes. The checkpoint taken before the run's first change already covers them, so `runs undo` puts a document file back as it was. The next hash check sees a different content hash and records a `system` revision "file restored by run undo". The revision log stays truthful and nothing is lost.
2. The document tools are **not** gated on a checkpoint existing, and they never ask the checkpoint to snapshot (a document write is its own safety net).
3. `.tern/` and `documents/.versions` are never used for revision storage; revisions live in the store, so the checkpoint's exclusions (`.tern/`, `.gitignore`) do not matter to them.
4. If the user's folder is a git repo, tern does not commit or stage anything; the user's git sees the file change as any editor's would.

## 3. Agent tools

Six tools, in a new `crates/tern-tools/src/documents.rs`, all file access through the scope, all store access through a `DocumentRepo`.

### 3.1 The tools

```jsonc
create_document  { "title": str, "content": str, "kind"?: "markdown|text|code|csv|html",
                   "path"?: str, "note"?: str }
  -> { "doc_id", "path", "rev": 1, "hash", "bytes" }

read_document    { "doc": id|path, "rev"?: int, "range"?: {"offset_line": n, "limit": n} }
  -> { "doc_id", "title", "rev", "hash", "content", "truncated", "tainted", "open_in_panel": bool }

edit_document    { "doc": id|path, "base_rev": int,
                   "edits": [ {"find": str, "replace": str, "occurrence"?: int} ]   // or
                   "edits": [ {"insert_after": str|null, "text": str} ]
                   "note"?: str }
  -> { "rev": new, "applied": n, "rebased": bool }
  | error { "kind": "stale", "base_rev", "current_rev", "changed_regions": [ {before, after, lines} ], "current_text"?: str }
  | error { "kind": "no_match" | "ambiguous", "find": str, "candidates": [ {line, context} ] }

replace_document { "doc": id|path, "base_rev": int, "content": str, "note"?: str }   // full rewrite
list_documents   { "scope"?: "conversation|workspace|channel|all", "query"?: str, "limit"?: int }
  -> [ { "doc_id", "title", "kind", "path", "rev", "updated_at", "author_of_latest", "tainted" } ]
export_document  { "doc": id|path, "format": "md|html|pdf|docx|txt", "to"?: "exports/..." }
  -> { "path", "bytes" }
```

Notes:

- `edit_document` mirrors `edit_file` on purpose (`find` must match exactly once unless `occurrence` is given), so the model's habits carry over. `replace_document` exists because a full rewrite is a different intent from a patch, with a different warning ("this replaces every user edit since `base_rev`"), and a different approval wording if it ever has to ask.
- `read_document` returns `rev` and `hash`; the system prompt for the tools tells the model to pass the `rev` it last read as `base_rev`. Reads of an open document always return the newest saved revision, including unsaved-but-autosaved user edits (§4.3). An agent never sees unsaved keystrokes.
- There is **no `delete_document` for agents in v1.** Deleting is a user action (trash with undo). An agent that wants a document gone can empty it; the history keeps it recoverable.
- `export_document` writes the export file inside the workspace (`exports/` by default) and shows the user a card. Exports of `pdf` and `docx` need the exporters from D3; before then the tool offers `md`, `html`, `txt` only and says so in its spec.

### 3.2 Optimistic concurrency, precisely

State per document: `current_rev` (integer, monotonically increasing) and `content_hash`.

`edit_document(base_rev, edits)`:

1. Load the current revision. If `base_rev == current_rev`: apply the edits to the current text. All-or-nothing; on `no_match` or `ambiguous` nothing is written.
2. If `base_rev < current_rev` (someone wrote since the agent read): load text at `base_rev`, apply the edits **to that text** to get the agent's intended change, then three-way merge (base, agent's version, current) with a line-based merge (`diffy`) and a word-level pass when both sides touched the same line.
   - Clean merge: write a new revision with `rebased: true`, `parent_rev = current_rev`; the result tells the agent what the document looked like around its change so it can continue.
   - Conflict (overlapping hunks): **write nothing**. Return `stale` with the regions that conflict and the current text of those regions (cap 8 KiB), and a note: "The user changed this passage. Re-read, decide, then edit with the new `base_rev`."
3. If `base_rev > current_rev` or does not exist: an error, `bad_base`.

Reasons for the design: the model should never be able to destroy a user's work by being slow; clean merges are the common case (the user fixed a typo in paragraph 1 while the agent rewrote paragraph 5), and a hard stop with context is cheaper than a lost paragraph. `create_document` with an existing `path` that is already a document is refused (`exists`), pointing at `edit_document`.

The same primitive is used by the user's editor for saves: the client sends `base_rev` with every save (§6.2), and the core merges or returns a conflict (§4.3). One implementation, `documents::apply(base_rev, change, author)`.

### 3.3 Autonomy, approvals and taint

| Action | `readonly` | `supervised` | `code` / `autonomous` / `yolo` |
|---|---|---|---|
| `read_document`, `list_documents` | allowed | allowed | allowed |
| `create_document`, `edit_document`, `replace_document` (workspace file or store-only) | **refused** (no write) | **granted**, no prompt | granted |
| `export_document` to `exports/` or any confined path | refused | granted | granted |
| Any path outside the workspace | refused (never a question) | refused | refused |

This is the `write_file` policy (`ToolPolicy::granted_write(false).reaching(Reach::Workspace).taint_safe(confined)`): inside the folder the host has already granted the write, so documents **do not nag**. Every revision is undoable in the panel, which is the real reason a prompt is unnecessary.

**Taint.** The existing rule: in a tainted run, `write` effects ask unless the call is marked `taint_safe`, and `[taint] workspace_writes` (default true) honours `taint_safe` for confined writes. Documents follow that: a confined document write in a tainted run is `taint_safe` and does not ask, so a routine that summarises scraped pages can still write its report. What tainted writes add:

1. **Provenance.** A revision written by a tainted run has `tainted = 1` and `provenance` JSON: `{ "sources": ["https://example.com/x", "email:from dana@…"], "run_id": ..., "tainted_by": "web_fetch" }`, taken from the run's taint ledger (the same record that makes the approval prompt say "triggered by an email from X").
2. **The document's `tainted` flag** is true if any revision since the last **Mark as reviewed** was tainted. The panel shows a quiet "From the web" chip (underline-tab style, not a warning banner) with the sources on hover. **Showing a tainted document is always fine**; rendering rules (§5) are what make it safe to show.
3. **Reading back.** `read_document` on a tainted document wraps the content in the same untrusted-data delimiters and provenance note used for fetched content (`<untrusted source="document:ID rev:N" …>`), and **taints the reading run**, exactly as a tool result from outside the trust boundary does. An untainted document (user-written, or an agent write from an untainted run, or reviewed) reads back plain and does not taint.
4. **User edits do not clear taint**, because a one-character fix does not make the surrounding text safe. **Mark as reviewed** is an explicit user action (revision-level, ledgered) that clears the flag from then on.
5. **Export** of a tainted document keeps the chip in the export's front matter for `md` and `html` ("source: web, unreviewed"), a one-line comment, never in `pdf` or `docx` body text.
6. **The quarantined fetcher** (when it ships) may return a document directly; its output is tainted by definition.

Taint is not a gate on writing. It is a label that decides how the text is read later and what the agent that reads it may then do without asking.

### 3.4 Limits and the ledger

- **Content limits:** a single tool call may carry up to **256 KiB** of text (in or out); `read_document` truncates at 256 KiB with `truncated: true` and a `range` hint. A document holds up to **5 MiB** (hard refusal above; the panel warns above 1 MiB, §5.6). `edits` is at most 50 pairs per call. `list_documents` returns at most 100.
- **Rate:** at most 60 document revisions per run, to bound a runaway loop (configurable `[documents] max_writes_per_run`, refusal says why).
- **Ledger:** every revision by an agent or a user action that mutates (create, edit, replace, restore, rename, move, delete, visibility change, mark reviewed, export) appends a ledger entry: `kind = "document"`, `actor` = `agent:<id>` / `owner` / `user:<id>` (teams), `subject = doc_id`, `summary = "edit_document: rev 6 (+12 −3)"`, `detail` = `{rev, parent_rev, content_hash, bytes, path, run_id, tainted, rebased}`. Content is **not** in the ledger (the revision store has it); the hash ties them, so `tern ledger verify` plus a revision's hash proves the text. Coalesced user autosaves ledger once, when the coalescing window closes. Ledger-hash compatibility: only a new `kind`, no new columns, so existing chains verify unchanged.

### 3.5 How the agent knows what is open and selected

A new, lightweight **UI context**, owned by the client and held in memory by the core:

```
PUT /v1/sessions/{id}/ui_context
{ "documents": [ {"doc_id":"d_…","title":"Weekly report","rev":6,"active":true,
                  "selection": {"start": 1204, "end": 1388, "text": "…the paragraph…", "line_start": 31, "line_end": 34 },
                  "viewport_line": 28 } ],
  "updated_at": … }
```

- Sent by the desktop app on change (debounced 300 ms) while the conversation is selected. It is **ephemeral**: in memory, expires after 10 minutes without refresh, never stored, never in the ledger.
- When the user sends a message (or the agent's run starts for any trigger by that user), the core renders it into a short block **appended to that user turn**, not into the system prompt:

  ```
  <ui_context>
  Open documents: "Weekly report" (d_8f2, rev 6, active), "Notes" (d_31a, rev 2).
  Selected in "Weekly report", lines 31-34:
  > …the selected text…
  </ui_context>
  ```

  Reason: the system prompt and tool list are the **cached prefix** (the prompt-caching work, PR #42); anything that changes per turn goes at the tail so it never invalidates the cache. A selection of more than 4 KiB is trimmed to its first and last 1 KiB with line numbers.
- **"Edit this paragraph".** The panel's selection toolbar offers **Ask tern…** (a small inline prompt, ⌘K inside the document) and **Rewrite**, **Shorten**, **Explain**. These send a normal message with the selection already in `ui_context`; the model answers with an `edit_document` call whose `find` is the selected text. Because the find text came from the visible revision and `base_rev` is the revision it was read at, the edit is safe even if the user keeps typing.
- **Who can see it.** Only the run for that user's message. A routine run or another member's run never gets `ui_context`.
- Taint note: `ui_context` is typed by the user in their own app and is trusted, **except** the selected text, which is document content and inherits the document's taint (a selection from a tainted document is wrapped as untrusted).

## 4. Live editing UX

### 4.1 Streaming the agent's writing into the panel

Event flow for `create_document` or `replace_document`:

1. `tool_started` for the call arrives as today. If the provider streams tool-call JSON (Anthropic's `input_json_delta` does), the model adapter incrementally extracts the `content` string and emits `document_stream` events: `{ "doc_id" (or "pending": true with title/path), "run_id", "op": "begin|append|end", "text": "…", "offset": n }`.
2. The panel opens (or switches to) the tab on `begin`, shows a streaming caret and appends text to a **provisional buffer** with a quiet "tern is writing…" label. The user can keep reading; scrolling is not hijacked after they scroll up (the same hold-position rule as chat, PR #33).
3. On `tool_finished`, the core commits the revision and sends `document_updated` with the final rev. The panel swaps the provisional buffer for the committed text (identical by construction) and highlights it (§4.2). If the call fails or the run stops, the buffer is discarded with "Stopped, nothing was saved" and the tab shows the previous revision.
4. **Fallback when the provider does not stream tool arguments:** the text appears when the call finishes, revealed with a short top-to-bottom animation (≤ 600 ms) so the user sees movement. This is the D1 behaviour; true streaming is D2b.
5. For `edit_document` there is nothing to stream: the patch is small. The panel scrolls to the first changed region and highlights it.

Streaming never writes to disk or to the revision log. The file is written **once**, at commit.

### 4.2 What changed: highlight

- After any revision from someone else (agent or external), the panel marks the changed lines with a sand-tinted background and a left rule for **8 seconds**, then fades to a small gutter dot that stays until the user clicks it or edits that region. Deleted text shows as a thin struck strip at the position, dismissable. Colour never carries meaning alone: the gutter dot and a "Changed by Research agent · 12 s ago" line sit in the panel's status row.
- **Changes** button in the panel's top bar shows "+12 −3 since you last looked" and toggles an inline diff (unified, in the same column) against the revision the user last viewed. A **History** tab (underline tab next to Document) offers the full revision list and a two-revision diff (side by side in full-screen).
- The "last viewed" revision is per user and document and kept client-side with a sync to the core (`document_reads`, a small table, §6.7) so it works across devices.

### 4.3 User editing, autosave and conflicts

- **Edit mode.** A **View / Edit** segmented control at the top of the document (View is the default for agent-written docs; Edit for ones the user created). Edit mode is a plain Markdown source editor (`TextField`, monospace, line numbers optional) with a **split live preview** (side by side when the panel is ≥ 720 px, a toggle otherwise). A small toolbar inserts Markdown syntax (heading, bold, italic, link, list, quote, code). See §5.5 for why not a WYSIWYG package in v1.
- **Autosave.** Debounce 1.5 s idle, or on blur and ⌘S, sends `PUT /v1/documents/{id}` with `base_rev` and the full text (or a patch for big documents). A subtle state next to the title: "Saving…", "Saved", "Offline, will retry" (the core being unreachable queues the save in memory, with the buffer kept). Closing a tab with an unsaved buffer saves first.
- **Agent writes while the user is typing.** The agent's tool result path calls the same `apply` with its `base_rev`; the user's un-saved keystrokes are not on the server, so:
  - Agent edit lands, user's next autosave has a stale `base_rev`. The core three-way merges (user's version vs base vs current). Clean: save succeeds, the client receives `merged: true` and the merged text and **replaces its buffer while preserving the caret** by mapping the offset through the merge. A toast says "Merged with tern's edit".
  - Overlapping change (the user is typing inside the paragraph the agent just rewrote): **the server refuses and returns the conflict**. The client keeps the user's buffer, shows the conflict bar: "tern changed this paragraph while you were editing it." with **Keep mine**, **Take tern's**, **Compare**. Compare opens the inline diff. Neither side is lost: the revision with the agent's text exists, the user's text is still in the buffer, and *Keep mine* saves it as the next revision on top.
- **No lock.** The core does not block an agent edit while the user types (that would stall runs). Merge-or-conflict is the whole mechanism. A "you are editing, tern will wait" soft lock is a later nicety (open question 7).
- **Undo.** In edit mode, ⌘Z is the editor's native text undo (per keystroke, within the buffer). Past a save boundary or an agent edit, undo goes to **History**: the top bar has **Undo tern's edit**, which restores the previous revision as a new revision (one click, with a toast to redo). This keeps text-undo simple and revision-undo explicit.
- **Find** (⌘F) in View mode highlights matches in the rendered text (or the source toggle); in Edit mode it uses the editor's find. A find-and-replace bar is in edit mode only, later.
- **Copy:** **Copy** copies Markdown source; **Copy as rich text** copies rendered HTML (clipboard `text/html` plus `text/plain`). Selection copy follows normal text rules.
- **Print:** ⌘P prints the rendered document through the export-to-PDF path (D3), with print CSS-equivalent styling (§5.7). Before D3, print is hidden.

### 4.4 Comments and suggestions (later phase, D4)

- **Comment** anchors to a text range with a quote and offsets resolved against a revision: `{ quote, prefix, suffix, rev }`. On later revisions the anchor is re-found by quote and context with fuzzy fallback; if it cannot be found, the comment is shown as "outdated" at the top, never dropped.
- **Suggestion** = a pending edit: `{ find, replace, rev, author }` stored as a `document_comments` row of kind `suggestion`. Agents create suggestions with `suggest_edit(doc, base_rev, find, replace, reason)`; the panel renders it inline as struck old text and underlined new text with **Accept** and **Reject** (and Accept all). **Accept** applies it as the user's revision via the normal `apply` (so it merges or conflicts like any edit). **Reject** marks the row rejected and lets the agent know on the next turn.
- **Suggesting mode** is a per-document toggle ("Agent edits as suggestions"): when on, `edit_document` calls by agents on that doc are turned into suggestions instead of revisions, so the user stays in control. The agent is told so in the tool result ("proposed, not applied"). This is the answer for people who find direct edits uncomfortable, and is why it is the next phase after revisions, not before.
- Users can comment (`@tern, tighten this` posts to the conversation with the anchor). Comment threads are per-document and show in a side rail in full-screen, as a popover in the panel.

### 4.5 Export, from the panel

**Export** menu: Markdown (the source), HTML (rendered, self-contained, a minimal stylesheet, images inlined as data URIs if small), PDF, Word (.docx), plain text. **Copy**, **Reveal in Finder** (file-backed), **Open in editor** (`$EDITOR` or system default). **Save to workspace…** for store-only.

## 5. Rendering and security

### 5.1 Threat model

Document text can come from the model, from a web page the model summarised, from an email, from a teammate, or from an imported file. Anything that renders it is a place where hostile text can try to do something: run script, phone home to leak what is on screen (tracking pixels, remote images, `<link>`), spoof UI, or steal a click (a link that looks like a button). The model for Markdown and HTML alike: **a document is data, rendering is a pure function from text to widgets, and it has no side effects the user did not choose.**

### 5.2 Markdown (the default path, no web view)

- Parse with `package:markdown` (GFM) and render as native Flutter widgets with `flutter_markdown_plus`, which `apps/desktop` already depends on (and `package:markdown` is already in the lockfile). Native widgets mean **no script engine exists to attack**.
- **Raw HTML inside Markdown is not rendered.** The `markdown` parser is run without `inlineHtml` support; the existing `tidyMarkdown` stripping of tags in `markdown_text.dart` is reused and made stricter (tags dropped, `<br>` to newline, contents kept as text). Comments (`<!-- -->`) are dropped. This is the safe default; a user who wants HTML opens an `.html` document (§5.3).
- **Images** in Markdown: `![alt](path)` with a **relative path** resolves through the document's workspace scope (core API `GET /v1/documents/{id}/assets/{path}`, confined, size-capped at 10 MiB, image MIME only: png, jpeg, gif, webp, and svg only through the sanitiser). **Remote images (`http`, `https`) are not fetched**: a placeholder "Image from example.com, Load" appears; **Load** fetches it once through the app's HTTP client (not the webview), with a 10 MiB cap and no cookies, for this session. A per-document **Always load images from this document** toggle is a later convenience, off by default; tainted documents never offer it. `data:` images are decoded locally if ≤ 1 MiB and image MIME. Reason: a remote image is the simplest data-exfiltration channel (it leaks that the user opened the doc and, with query strings, any text the model put in the URL).
- **Links.** Rendered as text with the underline. A click opens a confirm sheet for `http(s)` and `mailto` links: "Open example.com in your browser?" showing the **full URL** (with punycode decoded if the host is IDN and flagged) and **Open** and **Copy link**; a "don't ask again for this host" option is available for hosts the user chose, never for tainted documents. Other schemes (`file:`, `javascript:`, `data:`, `tern:` app links we did not create) are inert text. Relative links to another document in the workspace open it in a tab. Links open in the system browser, never in-app.
- **Code blocks** are highlighted with a small Dart highlighter (see §5.5 licence table) and have a **Copy** button. They never execute.
- **Mermaid** (§5.4) and **tables** (rendered natively; a wide table scrolls horizontally inside its own box).

### 5.3 HTML documents (sandboxed, v1.1)

HTML is untrusted whether the agent wrote it or it came from the web. The rule is **defence in depth: sanitise, then sandbox, then deny network**.

1. **Sanitise in the core, not the client**, with `ammonia` (Rust, MIT or Apache-2.0): allow structural and text tags, tables, `img` (relative only), `a` (`href` for `http`, `https`, `mailto`, and `#`), `style` **attributes** limited to a safe property list, a `<style>` element with a CSS filter that removes `@import`, `url(...)` with non-data schemes, `expression`, `behavior`, `-moz-binding`. Remove: `script`, `iframe`, `object`, `embed`, `form`, `input`, `button`, `link`, `meta`, `base`, `svg` (until separately handled), event-handler attributes, `srcdoc`, `javascript:` URLs.
2. **Render in a sandboxed web view** with: JavaScript **disabled**; navigation blocked after first load (every link click is intercepted and goes through the confirm sheet and the system browser); a **Content-Security-Policy** `default-src 'none'; img-src data: ; style-src 'unsafe-inline'; font-src data:;` injected into the sanitised HTML; no cookies or storage (ephemeral data store); no access to the clipboard or camera; the content loaded from `loadHtmlString` with a `about:blank`-like base so relative URLs resolve to nothing. Relative images are rewritten by the core to `data:` URIs (size-capped).
3. **No network from the document**, ever, without the explicit **Load** (§5.2), and then only for images, through the app's client.
4. **Source toggle** shows the original, unsanitised text, as text, with a note when sanitising removed something ("2 scripts and 1 external stylesheet were removed").
5. **Web view availability:** `webview_flutter` has a solid macOS implementation (WKWebView, BSD-3). On Linux there is no first-party implementation, so Linux v1 shows HTML as **source plus a sanitised native preview** built with `flutter_widget_from_html_core` (MIT, no web view; supports a useful subset), and gets the web view when a Linux implementation is judged stable. This also lets us **ship HTML without any web view at all** if we choose the native subset everywhere (open question 3: default = macOS web view, Linux native subset).

### 5.4 Mermaid

No maintained, pure-Dart Mermaid renderer exists. Options:

| Option | Licence | Cost | Verdict |
|---|---|---|---|
| Mermaid.js (MIT) in the same sandboxed web view with a bundled copy of `mermaid.min.js` and strict CSP (`script-src` a nonce for our bundled script only) rendering **into SVG**, then the SVG is sanitised and displayed natively (`flutter_svg`, BSD-3) | MIT | ~2.5 MB asset; web view needed (macOS) | **Recommended** where a web view exists. Diagram source is data passed as JSON, never as HTML; `securityLevel: 'strict'` and `htmlLabels: false`. |
| Core-side render with a Rust crate (`mermaid-rs-renderer` and similar, young) | verify | no web view | Revisit; not trustworthy yet for flowcharts plus sequence plus gantt. |
| Show source only | n/a | none | **Fallback** (Linux, or if the renderer fails): the fence is shown as a code block with the label "Diagram source". |

Diagram rendering is **D3c**, behind the same security rules as HTML. A parse error shows the error and the source, never a blank.

### 5.5 Flutter options compared, with licences

All licences are listed from memory of each package's `LICENSE` file and **must be re-verified in the D1 PR** (`flutter pub deps`, `pub.dev` page, `LICENSE` file; add the check to `docs/release-checklist.md` as it is for Rust's `cargo deny`).

**Markdown rendering**

| Package | Licence | Notes | Verdict |
|---|---|---|---|
| `flutter_markdown_plus` (current dep, community fork of the discontinued `flutter_markdown`) | BSD-3 | Native widgets, builders for custom blocks (code, mermaid, table), selectable | **Use.** Already shipped, already tuned (`markdown_text.dart`). |
| `flutter_markdown` | BSD-3 | Discontinued by the Flutter team | Do not add. |
| `markdown_widget` | MIT | Richer out of the box (TOC, code highlighting, selection) | Option if `flutter_markdown_plus` fights us on tables or selection; adds a second renderer, so not now. |
| `flutter_widget_from_html(_core)` | MIT | HTML to native widgets, no web view | **Use for the Linux HTML subset** only. |
| `webview_flutter` (+ `_wkwebview`) | BSD-3 | WKWebView; JS can be disabled; needs CSP via injected `<meta>` | **Use for sandboxed HTML and Mermaid on macOS.** |
| `flutter_inappwebview` | Apache-2.0 | More control (request interception), heavier, macOS and Linux unofficial | Fallback if `webview_flutter` cannot block navigation or inject CSP the way §5.3 needs. |

**Editor**

| Package | Licence | Model | Verdict |
|---|---|---|---|
| Plain `TextField` / `EditableText` + live preview (no package) | n/a | Markdown source | **Use for v1.** Zero dependency, Markdown stays the source of truth, what the agent writes and what the user edits are the same bytes (which is what makes merging and diffing sane). Cost: no WYSIWYG, selection offsets are source offsets (good for `ui_context`). |
| `super_editor` | MIT | Document-model editor with Markdown import and export | Strong candidate for a **later WYSIWYG mode** (D2c). Round-tripping Markdown through a rich model can rewrite the user's source (list markers, spacing), which breaks diff and the file-is-the-truth promise. Needs a spike. |
| `flutter_quill` | MIT | Delta rich-text; Markdown only via converters | Not recommended: Delta is a second source of truth. |
| `fleather` | MIT | Parchment rich-text | Same objection. |
| `appflowy_editor` | **AGPL-3.0** (verify) | Block editor | **Do not use**: a network-copyleft licence in a product with a hosted core. |
| Code editors (`re_editor`, `flutter_code_editor`) | MIT / verify | Syntax-highlight plus line numbers | Candidate for `code` kind edit mode; the plain `TextField` plus `re_highlight` (MIT) overlay is enough to start. |

**Recommendation:** `flutter_markdown_plus` for display, a plain Markdown source editor with live split preview for editing, `webview_flutter` for the sandboxed HTML/Mermaid path (macOS), `flutter_widget_from_html_core` for Linux HTML, `re_highlight` for code. Spike `super_editor` as D2c after the alpha.

Rust side (core): `similar` (Apache-2.0) line and word diff, `diffy` (Apache-2.0 or MIT) three-way merge, `ammonia` (MIT or Apache-2.0) HTML sanitiser, `pulldown-cmark` (MIT) for Markdown to HTML in export, `zstd` (MIT or Apache-2.0, already vendored with the store or add), `blake3` (CC0 or Apache-2.0, already in use), `docx-rs` (MIT) for Word, `printpdf` or `typst` (Apache-2.0) or the webview's print-to-PDF for PDF (D3 spike; §5.7). **Not** pandoc (GPL, external binary): a user may have it and we may offer "export with pandoc if installed" later, never bundle it.

### 5.6 Large documents and performance

- Panel budget: opening a 100 KB Markdown renders under 100 ms; typing latency under one frame at 500 KB; panel idle CPU zero (SSE-driven).
- Rendering is **lazy by block**: the Markdown is parsed into blocks (heading, paragraph, list, code, table) in an isolate (`compute`) when above 50 KB; blocks render in a `ListView.builder` so only visible ones build. A table over 500 rows is virtualised. A code block over 2,000 lines collapses with "Show all".
- Above **1 MiB** the viewer renders as source with a note ("large document, shown as plain text"); editing is allowed in plain mode. Above **5 MiB** the core refuses writes.
- Streaming appends rebuild only the **last block**, not the whole tree.
- Diff on large documents is computed in the core (line diff first, word diff only inside changed lines under a size cap) and the client only renders the hunks it was sent.
- Live preview in edit mode is debounced 150 ms and parsed off the UI isolate above 50 KB.

### 5.7 Export implementation sketch

- `md`, `txt`: the bytes. `html`: `pulldown-cmark` rendering, then `ammonia` (so the export is safe to open), a small inline stylesheet from the brand tokens, no scripts. A tainted source adds the front-matter comment (§3.3).
- `docx`: Markdown AST to `docx-rs` (headings, paragraphs, lists, tables, code as monospace runs, links, images from the workspace). Fidelity is "clean and editable", not pixel match. Spike in D3.
- `pdf`: two candidates. (a) print the sanitised export HTML with the platform web view's print-to-PDF (macOS WKWebView has it; zero new dependency; Linux lacks it). (b) a core-side renderer (`typst` as a library, or `printpdf` with our own layout): consistent on every platform, more work. **Recommendation:** (a) for the alpha path on macOS, with (b) tracked if a hosted core must export (open question 5).

## 6. API and protocol

### 6.1 REST

Under the existing auth (`Auth::Account`, device tokens, local socket). Conventions as the other `api_*.rs` modules; new module `crates/tern/src/daemon/api_documents.rs`.

```
GET    /v1/documents?scope=&session=&channel=&agent=&q=&kind=&limit=&cursor=     list (Viewer-scoped)
POST   /v1/documents                      { title, kind?, content, path?, workspace?, session?, channel?, agent? }
GET    /v1/documents/{id}                 metadata + current text + rev + hash + tainted + provenance
PUT    /v1/documents/{id}                 { base_rev, content | patch, note? }  -> 200 {rev, hash} | 200 {rev, merged:true, content} | 409 {conflict}
PATCH  /v1/documents/{id}                 { title?, path? (rename/move), visibility?, archived? }
DELETE /v1/documents/{id}                 soft delete (trash); ?purge=true for owner/admin after trash
POST   /v1/documents/{id}/restore         { rev } -> new revision
POST   /v1/documents/{id}/reviewed        clear the tainted flag
GET    /v1/documents/{id}/revisions       list (rev, author_kind, author_id, run_id, at, summary, label, bytes)
GET    /v1/documents/{id}/revisions/{rev} content of one revision
GET    /v1/documents/{id}/diff?from=&to=  hunks (line + word)
POST   /v1/documents/{id}/revisions/{rev}/label   { label }
POST   /v1/documents/{id}/export          { format, to? } -> file (stream) or { path }
GET    /v1/documents/{id}/assets/{path}   confined image/asset read
POST   /v1/documents/open                 { workspace, path }  bind an existing file (in place) -> document
POST   /v1/documents/import               { workspace?, name, bytes }  copy in (reuses the uploads path rules)
PUT    /v1/sessions/{id}/ui_context       open docs, active doc, selection (ephemeral)
POST   /v1/documents/{id}/read            { rev }  mark last-viewed (cross-device "since you last looked")
# D4
GET/POST /v1/documents/{id}/comments      list, add (comment or suggestion)
POST   /v1/documents/{id}/comments/{c}/accept|reject|resolve
```

Status codes: `409` on a conflicting save with the body `{ "error": "conflict", "base_rev", "current_rev", "regions": [...], "current_content" }`; `413` over limits; `403` per policy; `404` indistinguishably for "no such document" and "not visible to you" (teams rule).

### 6.2 Event stream

Events join the existing SSE stream. They are **per session** (for the conversation open in the app) and also fan out on a **document stream** `GET /v1/documents/{id}/events` for a panel that is open on a document from elsewhere (library, another conversation). Naming follows the existing snake-case `type` tag (`tool_started`, `run_finished`); the task wording "document.created" maps to `document_created`.

```jsonc
{ "type": "document_created",  "doc_id", "title", "kind", "path", "rev": 1, "author": {"kind","id"}, "run_id"?, "session_id"? }
{ "type": "document_stream",   "doc_id"|"pending", "run_id", "op": "begin|append|end|abort", "text"?, "offset"? }
{ "type": "document_updated",  "doc_id", "rev", "parent_rev", "author": {...}, "run_id"?, "summary", "changed": [ {"from_line","to_line"} ], "rebased": bool, "tainted": bool }
{ "type": "document_renamed",  "doc_id", "title", "path" }
{ "type": "document_deleted",  "doc_id" }
{ "type": "document_visibility", "doc_id", "visibility" }
{ "type": "document_comment",  "doc_id", "comment_id", "kind": "comment|suggestion", "op": "added|accepted|rejected|resolved" }   // D4
```

`document_updated` carries the summary and changed line ranges (small), **not** the text; the client fetches `GET /v1/documents/{id}` or, when it is the active panel, applies `changed` from a fresh fetch. This keeps events small and the stream safe to replay on reconnect (the existing `Last-Event-ID` behaviour). Events respect `Viewer` scoping: a member only receives events for documents they can see.

### 6.3 Dart client (`apps/desktop/packages/tern_client`)

New `src/documents.dart`: `DocumentsApi` (list, get, create, save with `SaveResult.{saved, merged, conflict}`, revisions, diff, restore, rename, delete, export, open, import, uiContext), models (`DocumentMeta`, `DocumentRev`, `DiffHunk`, `Conflict`), and new `AgentEvent` variants in the sealed event hierarchy. Desktop state: `lib/state/documents.dart` (open tabs per conversation, buffers, save state, last-viewed revisions) and UI: `lib/ui/documents/` (panel, tab bar, viewer, editor, history, diff, conflict bar, library). The right pane's mode switch is added to `side_panel.dart`.

### 6.4 TUI

- `/docs` lists documents of the conversation (title, rev, author of latest, tainted mark). `/doc <n or title>` shows it **as text** in the scrollback pager (Markdown shown plain with simple heading and list styling, no images, tables as aligned text).
- `/doc edit <n>` opens the file in `$EDITOR` (file-backed) or a temp copy for store-only; on exit the TUI saves through `PUT` with the `base_rev` it opened (so a concurrent agent edit merges or reports a conflict as text). `/doc history <n>` and `/doc diff <n> <a> <b>` print revisions and a unified diff.
- A document card appears in the transcript where the tool ran; live changes print a one-line "Weekly report updated (rev 6 by research, +12 −3)". CLI: `tern docs ls|show|open|history|diff|restore|export`.

### 6.5 Telegram

- When a run creates or updates a document in a Telegram conversation, the gateway sends the file as a **document attachment** (`sendDocument`) with the title as caption: `.md` as is, plus a PDF when D3 ships (user setting). Updates in the same conversation within a run send **one** file at the end of the run, not per revision. Edits to existing documents send the new file with "updated (rev N)".
- The user can send a text file to the bot; it is imported (`uploads` path) and the agent is told the `doc_id`. A caption "edit this" makes it an `edit_document` task. Limits: Telegram's 50 MB cap for bots; documents are far below it; refuse over 5 MiB.
- `/doc` commands list the latest documents with inline buttons (Get file). No inline editing in Telegram.
- A tainted document sent to Telegram is sent as a file like any other; the file itself is inert text. The caption says "from the web, unreviewed" for tainted ones.

### 6.6 Remote and hosted cores, and teams sharing

- **Remote cores** (TCP plus TLS, device tokens): documents are core-resident; the same REST API and events work unchanged. A hosted core has no user-visible folder in the usual sense, so its documents are **store-only** unless the user mounted a workspace; the panel's "Reveal in Finder" is hidden and **Download** and **Export** are the way out. The file watcher and external-change revisions are file-mode only.
- **Teams** (teams-design.md, M1): documents get the standard two columns, `owner_user_id` and `visibility` (`private | team`), plus `team_id`. Rules, all enforced by `tern_core::policy::decide` and a `Viewer`-scoped `DocumentRepo` (an unscoped list is a compile error):
  - **Private** (default): the owner only. A document's audience is its own `visibility`: set at creation to the channel's visibility (private for a DM or a private channel, team for a team channel), then independent of it. Admins see existence (title, owner, size) but not content, like channels (teams §2.2).
  - **Team:** every active member can **view and edit** (revisions keep who wrote what; a team document is a shared file). Only the owner or an admin can change visibility, rename and delete. A read-only team share (`team_read`) is designed in (`visibility` is free text validated in code) and deferred (open question 4).
  - **Agents act for the requester.** An agent's write is authored `agent:<id>` with `requester_user_id` recorded; it succeeds only if the **requester** may edit the document. A routine writes as its `owner_user_id`.
  - **Revisions show people:** "Dana, 3 min ago" and "research agent (for Dana)". Authors are snapshotted by display name at write time as posts are.
  - **Presence** (who has it open) rides on the teams presence work (phase 2 of teams) and is not needed for the alpha; concurrency is handled by revisions and merges.
  - **Removal of a member:** their documents stay with their `owner_user_id` and tombstoned-user rules from teams §7.5 (an admin may reassign ownership); their revisions keep authorship.
  - **Provenance travels:** a tainted flag is part of the document, so a member opening a team document sees the same "From the web" chip.
- **Local mode:** principal `LocalOwner`, everything allowed, no columns visible. Behaviour is exactly as described in §1 to §5.

### 6.7 Storage schema and migration sketch

Provisional number `0035_documents.sql` (teams reserves 0031 to 0034; renumber at merge to the next free). All additive; the new tables are independent of the teams tables, but use the same `owner_user_id` and `visibility` columns so teams (if merged first) needs no rework. If teams has not merged when documents lands, `owner_user_id` is NULL read as `owner` and `visibility` defaults to `private`, as every teams column does.

```sql
CREATE TABLE documents (
  id TEXT PRIMARY KEY,                 -- 'd_<uuid7>'
  kind TEXT NOT NULL,                  -- markdown | text | code | csv | html (validated in code)
  title TEXT NOT NULL,
  storage TEXT NOT NULL,               -- file | store
  workspace_id TEXT REFERENCES workspaces(id),
  path TEXT,                           -- relative to the workspace; NULL for store-only
  mime TEXT,
  current_rev INTEGER NOT NULL,
  content_hash TEXT NOT NULL,          -- blake3 of the current content
  size INTEGER NOT NULL,
  tainted INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'ok',   -- ok | missing (file gone) | archived
  agent_id TEXT, channel_id TEXT, session_id TEXT, schedule_id TEXT,   -- origin (all nullable)
  owner_user_id TEXT, visibility TEXT NOT NULL DEFAULT 'private', team_id TEXT,
  created_by TEXT NOT NULL,            -- actor string as in ledger
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  deleted_at INTEGER                   -- soft delete; purge after 30 days
);
CREATE UNIQUE INDEX documents_path ON documents(workspace_id, path) WHERE path IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX documents_updated ON documents(updated_at DESC);
CREATE INDEX documents_channel ON documents(channel_id, updated_at DESC);
CREATE INDEX documents_owner   ON documents(owner_user_id, visibility);

CREATE TABLE document_revisions (
  doc_id TEXT NOT NULL REFERENCES documents(id),
  rev INTEGER NOT NULL,
  parent_rev INTEGER,                  -- NULL for rev 1
  author_kind TEXT NOT NULL,           -- agent | user | external | system
  author_id TEXT NOT NULL,
  requester_user_id TEXT,
  run_id TEXT, tool_call_id TEXT, schedule_id TEXT,
  blob_hash TEXT NOT NULL REFERENCES document_blobs(hash),
  size INTEGER NOT NULL,
  summary TEXT,
  label TEXT,                          -- user-named version; never thinned
  tainted INTEGER NOT NULL DEFAULT 0,
  provenance TEXT,                     -- JSON sources, only when tainted
  ledger_seq INTEGER,                  -- the ledger entry for this revision
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,   -- updated_at moves during user coalescing
  PRIMARY KEY (doc_id, rev)
);

CREATE TABLE document_blobs (
  hash TEXT PRIMARY KEY,               -- blake3 hex of the raw content
  zbytes BLOB NOT NULL,                -- zstd level 3
  raw_size INTEGER NOT NULL
);                                     -- orphan blobs purged by the retention job

CREATE TABLE document_links   (doc_id TEXT NOT NULL, session_id TEXT NOT NULL, first_seen INTEGER NOT NULL, PRIMARY KEY (doc_id, session_id));
CREATE TABLE document_reads   (doc_id TEXT NOT NULL, user_id TEXT NOT NULL, rev INTEGER NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (doc_id, user_id));

-- D4
CREATE TABLE document_comments (
  id TEXT PRIMARY KEY, doc_id TEXT NOT NULL REFERENCES documents(id),
  kind TEXT NOT NULL,                  -- comment | suggestion
  rev INTEGER NOT NULL,                -- the revision the anchor was made against
  anchor TEXT NOT NULL,                -- JSON {quote, prefix, suffix, start, end}
  suggestion TEXT,                     -- JSON {find, replace} for kind=suggestion
  body TEXT, author_kind TEXT NOT NULL, author_id TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open', -- open | accepted | rejected | resolved | outdated
  created_at INTEGER NOT NULL, resolved_at INTEGER
);

-- Routines: where a routine writes its output (D0 or later)
ALTER TABLE schedules ADD COLUMN output_document TEXT;   -- path or doc id; NULL = none
```

Search: an FTS5 table `documents_fts(title, body)` is maintained from `current` content on each revision (title and body only, content decompressed once), added in D2 so the Documents library and `list_documents(query)` can search. It follows the visibility rules by joining the Viewer filter.

Migration invariants: no existing table loses a column; the ledger gets a new `kind` only; a test opens a pre-migration database fixture and verifies the ledger chain before and after; the retention job is idempotent.

## 7. Phasing, work packages and test plan

### 7.1 Work packages

Owned files are the ones the package alone edits; "shared" files need a rebase when two packages land close together.

| # | Package | Owned files | Depends on | Parallel with | Estimate |
|---|---|---|---|---|---|
| **D0** | **Store, API, tools.** Migration, `DocumentRepo`, `documents::apply` (create, edit, replace, merge, revision coalescing, blobs), `GET/POST/PUT/PATCH/DELETE` plus revisions and restore, the six tools, policy and taint handling, ledger entries, events `document_created/updated/renamed/deleted`, `tern docs` CLI, external-change check on open. | `crates/tern-store/migrations/0035_documents.sql`, `crates/tern-store/src/documents_repo.rs`, `crates/tern-core/src/documents.rs` (apply, merge, diff), `crates/tern-tools/src/documents.rs`, `crates/tern/src/daemon/api_documents.rs`, `crates/tern/src/documents_cmd.rs`; shared: `events.rs`, `daemon/api.rs` routes, `tern-tools/src/lib.rs` registry, `cli.rs` | none | D1 (against a stubbed client) | 6 to 8 days |
| **D1** | **Panel viewer.** Dart client models and API, the right-pane mode switch (Activity / Documents), document tabs, Markdown viewer (native), kinds `text`/`code`/`csv`, the transcript card, Open from a card and from a path, Documents library list, view-only History list, copy, find, link confirmation, image placeholder rules, reveal-in-Finder, fallback reveal animation. | `apps/desktop/packages/tern_client/lib/src/documents.dart`, `apps/desktop/lib/state/documents.dart`, `apps/desktop/lib/ui/documents/*`; shared: `side_panel.dart`, `items.dart` (card), `sidebar.dart`, `markdown_text.dart` | D0 API shape (not its code) | D0, D3 spike | 6 to 8 days |
| **D2a** | **Editing and revisions UI.** Edit mode (source plus split preview, toolbar), autosave, merge and conflict bar, diff view, history restore, label, Undo tern's edit, "since you last looked" highlight, `ui_context` and selection toolbar (Ask tern, Rewrite, Shorten), the context block in the run. | `apps/desktop/lib/ui/documents/editor/*`, `.../diff.dart`, `crates/tern/src/daemon/ui_context.rs`, prompt addition in `tern-core/src/prompt.rs` (tail block) | D0, D1 | D3, D2b | 7 to 9 days |
| **D2b** | **True streaming.** Provider adapters emit incremental tool-argument text, `document_stream` events, provisional buffer. | `crates/tern-providers/*` (per adapter), `events.rs`, `ui/documents/stream.dart` | D0, D1 | D2a, D3 | 4 to 6 days (adapter by adapter; Anthropic first) |
| **D3** | **Export and print.** md, txt, html (D3a, small); pdf (D3-pdf, spike then build); docx (D3-docx); HTML kind with sandbox (D3b); Mermaid (D3c). Telegram file send, routine `output_document`. | `crates/tern-core/src/documents_export.rs`, `ui/documents/export.dart`, `ui/documents/html_view.dart`, `crates/tern-gateway/*` (file send), `crates/tern/src/routines.rs` (output) | D0 (export API), D1 (menu) | D2a/D2b | 3 days (md/html), 4 days pdf, 4 days docx, 5 days HTML sandbox, 4 days Mermaid |
| **D4** | **Comments and suggestions.** Table, API, `suggest_edit` tool, suggesting mode, inline rendering, accept and reject, anchors across revisions. | `document_comments` repo, `tern-tools/src/documents_suggest.rs`, `ui/documents/comments/*` | D0, D2a | D5 | 8 to 10 days |
| **D5** | **Teams sharing.** Visibility on documents through the policy layer, `Viewer`-scoped repo, member attribution in revisions, share toggle, team documents in the library, removal handling, presence hook. | `tern-core/src/policy.rs` (rules), `documents_repo.rs` (viewer), `ui/documents/share.dart` | D0, teams T1 (the policy and `Viewer` code) | D4 | 3 to 4 days after teams lands |

Parallelism: **D0 and D1 can start together** if D0 posts the API/event shapes in the first day (this doc's §6 is that contract; the first D0 commit is `api_documents.rs` types plus a fixture). D2a/D2b/D3/D4 are all separate files; each rebases on the shared files in the table. Total with one person: about 7 to 8 weeks; with D0/D1 in parallel and D2/D3 in parallel, about 4 weeks of calendar time.

### 7.2 Smallest shippable alpha slice: "alpha-6 Docs A" (D0 + D1 + half of D2a)

The user can say "write me a one-pager on X"; the agent calls `create_document`; a card appears in the chat; the panel opens beside it showing the rendered Markdown; the user switches to Edit, types, autosaves; the agent edits it later with `edit_document` and the user sees the changed lines highlighted; History lists every version and **Restore** works. All files are real, in `~/tern-workspace/documents/`.

In: D0 (all), D1 (viewer, tabs, library, card, fallback reveal animation), D2a minus diff view, `ui_context`, and split preview (those two follow immediately in "Docs B"). Out: streaming (D2b), export beyond Markdown (a **Reveal in Finder** and **Copy** are enough for the alpha), HTML, Mermaid, comments, teams, Telegram files. **Estimate: 3 weeks with two people on D0 and D1/D2a in parallel, 4 to 5 weeks alone.** Release notes line: "The agent can write documents next to the chat. You can edit them. Every change is a version."

### 7.2a As built (the MVP in this PR)

**In:** D0 and the read-only half of D1.

- **Store:** migration `0031_documents.sql` (`documents`, `document_revisions`, `document_blobs`; teams has not claimed 0031 yet, renumber at merge if it has). `documents_repo.rs`: create, compare-and-swap revision commit, list, revisions, review mark, one ledger entry per write, in one transaction. Text is stored uncompressed and content-addressed by sha256; compression is a later, invisible change.
- **Tools** (`tern-tools/src/documents.rs`): `create_document`, `read_document`, `edit_document`, `list_documents`. `edit_document` takes `edits` (find/replace, each `find` unique) or `content` (full replace, only on the latest revision). A stale `base_rev` is rebased by line diff (`similar`) when the touched lines are unchanged, else refused with the current text of those lines. A hand edit of the file becomes an `external` revision first. Writes are granted inside the workspace (`taint_safe` when confined), never overwrite a file that is not a document, and are capped (256 KB per call, 5 MB per document, 60 revisions per run). A tainted run's document is flagged with provenance and read back `Untrusted`.
- **API:** `GET /v1/documents`, `/{id}`, `/{id}/revisions[/{rev}]`, `POST /{id}/reviewed`; health feature `documents`.
- **Live refresh:** no new event type yet. The app listens for `tool_finished` of `create_document` and `edit_document` on the run stream (the tool output carries `doc_id` and `rev`), refreshes that document, and opens a document created in the conversation on screen. The `document_*` events of §6.2 are the follow-up.
- **Panel:** the right pane's Activity / Documents underline tabs, a tab per open document with an unseen dot, a library list, rendered Markdown (`flutter_markdown_plus`, raw HTML dropped, images never loaded, link confirm), text, code and a CSV grid, a "From the web" bar with **Mark as reviewed**, a History tab with older revisions viewable read-only, Copy.
- **Differences from the design:** streaming is not built (the text appears when the call finishes); documents above 200 KB render as plain text (not 1 MiB) because the Markdown widget is not lazy; there is no transcript card yet; `external` revisions are noticed when an agent next edits, not by a file watcher; `read_document` returns the latest stored revision, not the file.

**Not built, with clean seams:** user editing and autosave (the revision commit already takes `expect_rev` and an author kind of `user`), diff view (`similar` is in the tree; `added`/`removed` are stored per revision), export, HTML and Mermaid preview, comments and suggestions (a new table and tool), teams sharing (`owner_user_id` and `visibility` columns exist, unused), streaming (`document_stream`), the transcript card, TUI and Telegram, routine `output_document`, and the file watcher.

### 7.3 Test plan

**Core (Rust):**
- `documents::apply` property tests: random concurrent edit pairs on random text. Invariant: no accepted write ever drops a character that neither side changed; a conflict writes nothing; applying the same edit twice is a no-op or a clean refusal.
- Merge: golden cases for same-line, adjacent-line, overlapping, insert-at-same-point, whole-file replace against edit, CRLF vs LF, trailing newline, Unicode (combining marks, emoji), empty document.
- Revisions: coalescing windows (user within 5 min, agent breaks the window), restore creates a new revision, thinning keeps labelled and agent revisions, blobs deduped and purged only when unreferenced, a crash between file write and revision insert recovers on next open (file hash differs, so an `external` revision is recorded; the **file is written before the revision** and the open check heals the other order).
- Files: confinement (`..`, symlink, hardlink, deny-set, absolute path, case-folding on macOS), rename and move, external edit and delete detection, size limits, atomic write (write temp, fsync, rename, through the scope).
- Policy: the autonomy table (§3.3) per level; taint table (tainted write allowed with provenance, tainted read wraps and taints, user edit does not clear, reviewed clears); `[taint] workspace_writes = false` makes tainted writes ask.
- Ledger: entry per mutation, `tern ledger verify` passes on a pre-migration fixture and after documents are written; hash chain unaffected.
- Teams (D5): `Viewer` scoping table-driven, 404 for invisible documents, requester-authority for agent writes, event filtering.
- Migration: forward-only on a fixture DB from the last release; idempotent bootstrap.
- Tools: spec snapshot tests (names, schemas) next to the existing tool-spec tests; `edit_document` error shapes (`no_match`, `ambiguous`, `stale`) asserted verbatim because prompts depend on them.

**API (integration, `crates/tern/tests/it/` as a new module `documents`, not a new binary):** CRUD, 409 conflict body, revisions list, diff, restore, import/open-in-place, events over SSE with replay, `ui_context` expiry and its rendering in the turn, visibility, 404 vs 403.

**Dart:** `flutter analyze` and widget tests (CI only, per CLAUDE.md): tab bar behaviour, save state machine (saving, saved, conflict, offline), merge replaces buffer and keeps caret (unit-test the offset mapping), Markdown rendering golden strings for hostile input (raw HTML, `javascript:` links, remote images, `data:` URIs, huge tables), link confirm sheet shows the full URL, stream buffer discarded on abort, diff rendering from hunks. Ask the owner to look at layout in the app, as for other UI work.

**Security checks:** a corpus of hostile Markdown and HTML (script tags, `<img onerror>`, `<style>@import url(...)`, `<meta refresh>`, SVG with script, `javascript:`/`data:` links, IDN homograph links, CSS `url()` exfiltration, giant nested lists) in the core sanitiser tests and in a widget test that asserts **no network call is made** (the HTTP client is a fake that fails the test if hit). Fuzz the Markdown block splitter and the merge function.

**Manual:** the weekly-report routine end to end (revisions across runs, diff between weeks), editing while the agent writes, killing the daemon mid-write, opening a file changed in another editor, a 2 MB document, Telegram file delivery.

## 8. Decisions summary

| Question | Decision |
|---|---|
| Files or store | Both: record plus file, store-only when no workspace |
| Revision store | SQLite, content-addressed zstd snapshots; not git |
| Relationship to checkpoint | Separate; undo-run restores bytes, next check records a system revision |
| Concurrency | `base_rev` plus three-way merge; conflicts write nothing |
| Approval | Granted inside workspace, refused in `readonly`; no delete tool for agents |
| Taint | Allowed, provenance recorded, reads wrap and taint, user edits do not clear, explicit review does |
| Markdown renderer | `flutter_markdown_plus`, no raw HTML |
| Editor | Plain source plus split preview; spike `super_editor` later |
| HTML | Sanitise in core, sandboxed JS-off web view on macOS, no network |
| Mermaid | Sandboxed Mermaid.js to sanitised SVG on macOS; source fallback |
| Streaming | `document_stream` events from tool-argument deltas; reveal animation as fallback |
| Teams | Document-level `visibility`; team means view and edit; agents act for the requester |

## 9. Open questions

Each has a default. **Settled on 2026-11-10** by the owner: Q1 (`documents/` under the run's workspace root; a routine's output may override later), Q6 (reading a tainted document taints the run; documents carry provenance and the panel marks them), Q14 (the tab is called Documents). All other defaults below were taken for the MVP (no agent `delete_document`, remote images never auto-load, the ledger records hash and counts). Still open for later phases: Q2 to Q5, Q7, Q8, Q11 to Q13.

1. **Which workspace subfolder is the default home for documents?** Options: `documents/` under the run's workspace root, or the workspace root itself. **Default: `documents/`** (keeps the user's folder tidy; a routine's `output` can override).
2. **Should user edits in the panel create per-save revisions or coalesce?** Default: coalesce within 5 minutes by the same author (§2.3). A "Save version" button makes a hard boundary.
3. **Ship HTML preview with a web view on macOS, or the native HTML subset everywhere?** Default: **macOS web view with JS off and CSP, Linux native subset.** Revisit if `webview_flutter` cannot block navigation and inject CSP as §5.3 requires (then `flutter_inappwebview` or native subset only).
4. **Teams: is a `team` document editable by all members, or view-only by default?** Default: **editable by all members** (it is a shared file; revisions show who). Add `team_read` when asked.
5. **PDF export engine: web-view print on macOS, or a core-side renderer so a hosted core can export?** Default: **web-view print for the alpha path; core-side (`typst` as a library) only if hosted cores need export.**
6. **Does reading a tainted document taint the run?** Default: **yes**, consistent with tool results from outside the boundary (§3.3). The cost is routines that read their own prior report: a routine's own previous untainted output stays untainted unless that run was tainted.
7. **Hard soft-lock while the user is typing ("tern will wait")?** Default: **no lock**; merge or conflict only. A later nicety if conflicts annoy.
8. **Revision retention numbers** (90 days full, then thinned; 1000 per document cap). Default as stated; make `[documents] retention_days` configurable.
9. **Does an agent get a `delete_document` tool?** Default: **no** in v1; the user deletes, to trash.
10. **Should remote images ever auto-load?** Default: **never automatically**; a per-document opt-in later, never for tainted documents.
11. **Should the ledger store a content hash only, or also a diff summary?** Default: **hash plus +/− counts**, text lives in revisions.
12. **Rich-text (WYSIWYG) editing mode**: worth the Markdown round-trip risk? Default: **not in v1**; spike `super_editor` after the alpha and ship only if it preserves the source byte-for-byte on untouched blocks.
13. **Does a routine's named output document keep one history across runs, or one document per run?** Default: **one document, one revision per run**, when `output_document` is set; otherwise one dated document per run.
14. **Naming of the panel tab.** "Documents" or "Canvas"? Default: **Documents** (plain, no jargon).

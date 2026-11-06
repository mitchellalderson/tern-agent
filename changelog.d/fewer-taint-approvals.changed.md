- **`http_fetch` returns data only by default.** With a URL and no `want`, the result is title, meta, headings, tables,
  selector hits and links, with no page prose, so the calling run stays untainted and later writes don't ask. Ask for
  `want: ["text"]` (or a `question`, for `answer`) when you need the prose; that taints the run as before. The tool
  description and four starter personas say so.
- **Writes inside the agent's own workspace no longer ask just because the run is tainted.** `write_file` and
  `edit_file` whose target resolves inside the run's folder (canonical path, no `..`, no symlink on the way, not a
  protected file) go through after the run read untrusted content; each is recorded in the ledger with the taint
  (`tainted-write-in-workspace`). Shell, channel posts, connector and MCP writes, memory, network reads of new sites
  and anything outside the folder still ask. Turn it off with `[taint] workspace_writes = false`.
- **Read-only shell pipelines are a read.** A command that is only a pipeline of plain reading programs (`cat`, `head`,
  `tail`, `od`, `hexdump`, `wc`, `grep` without `-r`, `ls`, `stat`, `cut`, `tr`, `nl`, `tac`, `rev`), with no redirect,
  `$()`/backticks, variable, glob, chaining or `-f`, and every operand inside the agent's workspace (symlinks and `..`
  resolved, protected files excluded), is classified `read` in a verified sandbox: it no longer shows as "destructive ·
  shell" and doesn't ask, including in a tainted run. Anything else, including any doubt, asks as before.

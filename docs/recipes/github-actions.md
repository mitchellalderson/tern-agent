# Recipe: wait for a GitHub Actions run

tern has no built-in GitHub connector. For `wait_until` you need one read tool: the status of a workflow run
(`GET /repos/{owner}/{repo}/actions/runs/{id}`). This is the connector; the agent can also write it from the
`connect-a-service` skill. Save it as `~/.tern/connectors/github.luau`.

```lua
return {
  name = "github",
  description = "GitHub: read the status of a workflow run",
  capabilities = {
    http = { "api.github.com/repos/*" },
    auth = "GITHUB_TOKEN",
    rate = "60/min",
  },
  tools = {
    run_status = {
      description = "Status of a GitHub Actions run: status (queued, in_progress, completed) and conclusion (success, failure, cancelled, …)",
      effect = "read",
      params = {
        type = "object",
        properties = {
          owner = { type = "string" },
          repo = { type = "string" },
          run_id = { type = "string", description = "The run's id, from its URL" },
        },
        required = { "owner", "repo", "run_id" },
      },
      run = function(ctx, args)
        local r = ctx.http.get("https://api.github.com/repos/" .. args.owner .. "/" .. args.repo
          .. "/actions/runs/" .. args.run_id, { headers = { Accept = "application/vnd.github+json" } })
        if not r.ok then error("GitHub answered " .. tostring(r.status)) end
        local run = ctx.json.decode(r.body)
        -- Only what a wait needs: the rest of a run is long and not yours to trust.
        return { status = run.status, conclusion = run.conclusion, name = run.name,
                 branch = run.head_branch, url = run.html_url }
      end,
    },
  },
}
```

Then the token: a fine-grained personal access token with read access to Actions on the repo, put in with
`tern secrets set GITHUB_TOKEN` (never in the chat or the file), and activate the connector (you see its hosts and
secret first).

## Using it

Ask: "Push this, then tell me when the CI run finishes." The agent calls

```json
{ "note": "report whether the CI run passed",
  "check": { "connector": "github", "tool": "run_status", "args": { "owner": "o", "repo": "r", "run_id": "123" } },
  "until": { "path": "status", "equals": "completed" },
  "every": "1m", "timeout": "1h" }
```

and stops. tern asks GitHub every minute (no model call). When the run completes, the same conversation gets a new turn
with the note and `{status, conclusion, …}` as untrusted data, and the agent tells you. `conclusion` is in the result, so
the agent can say whether it passed.

Without a connector: a public repo's run is readable with no token. `wait_until` takes
`{"check": {"url": "https://api.github.com/repos/o/r/actions/runs/123"}, "until": {"path": "status", "equals": "completed"}}`
(unauthenticated calls are limited to 60 an hour by GitHub, so use `every` of 2m or more).

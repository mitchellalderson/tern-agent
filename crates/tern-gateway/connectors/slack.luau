-- Slack as a tern gateway, with a bot token: no OAuth, no public URL.
--
-- Setup (`tern gateway add slack --channel C0123456789` prints this too):
-- 1. At https://api.slack.com/apps create an app "from scratch" in your
--    workspace. Under OAuth & Permissions add the bot scopes chat:write and
--    channels:history (groups:history for a private channel, im:history
--    for a DM), then "Install to Workspace".
-- 2. Save the Bot User OAuth Token (xoxb-…) as the secret SLACK_BOT_TOKEN.
-- 3. Invite the bot to the channel (/invite @yourapp) and put the channel's
--    id in settings.channel. Delivery targets are `slack:<channel id>`, or
--    `slack:<channel id>/<thread ts>` for a thread.
--
-- Messages are read by polling conversations.history, so tern needs no
-- reachable address. Replies inside threads aren't read yet: write in the
-- channel. Slack buttons need a public request URL, so approvals ask for a
-- numbered reply instead.

local API = "https://slack.com/api/"

local function call(r, what)
  local j = r.json
  if not r.ok or type(j) ~= "table" or not j.ok then
    local why = (type(j) == "table" and j.error) or ("HTTP " .. tostring(r.status))
    error("slack " .. what .. ": " .. tostring(why))
  end
  return j
end

local function post(ctx, channel, text, thread)
  return call(ctx.http.post(API .. "chat.postMessage", {
    json = { channel = channel, text = text, thread_ts = thread, unfurl_links = false },
  }), "chat.postMessage")
end

return {
  name = "slack",
  description = "Slack through a bot token: post messages, and the Slack gateway",
  capabilities = {
    http = { "slack.com/api/*" },
    auth = "SLACK_BOT_TOKEN",
    rate = "50/min",
  },
  tools = {
    post_message = {
      description = "Post a message to a Slack channel the bot is in",
      effect = "write",
      params = {
        type = "object",
        properties = {
          channel = { type = "string", description = "Channel id, e.g. C0123456789" },
          text = { type = "string" },
          thread_ts = { type = "string", description = "Reply in this thread" },
        },
        required = { "channel", "text" },
      },
      run = function(ctx, args)
        local j = post(ctx, args.channel, args.text, args.thread_ts)
        return { ts = j.ts, channel = j.channel }
      end,
    },
  },
  gateway = {
    kind = "slack",
    threads = true,
    send = function(ctx, to, text, opts)
      post(ctx, to, text, opts.thread)
    end,
    poll = function(ctx, cursor, settings)
      local channel = settings.channel
      if not channel then return { messages = {}, cursor = cursor } end
      -- First poll: start from now, not from the channel's history.
      local oldest = cursor or (tostring(os.time()) .. ".000000")
      local j = call(ctx.http.get(API .. "conversations.history?limit=100&channel=" .. channel
        .. "&oldest=" .. oldest), "conversations.history")
      local found, last = {}, oldest
      for _, m in ipairs(j.messages or {}) do
        -- Newest first. Skip bots (tern's own posts) and joins, edits, etc.
        if m.ts and m.ts > last then last = m.ts end
        if m.type == "message" and m.ts and m.ts > oldest and not m.bot_id and (m.subtype == nil or m.subtype == "thread_broadcast")
           and m.user and m.text then
          table.insert(found, 1, { id = m.ts, chat = channel, from = m.user, text = m.text })
        end
      end
      return { messages = found, cursor = last }
    end,
  },
}

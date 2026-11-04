-- ntfy (https://ntfy.sh) as a tern gateway: push notifications to your phone
-- with no bot or account, and replies through a second topic.
--
-- Installed by `tern gateway add ntfy`, which picks two random topic names.
-- Anyone who knows a topic name can read or write it, so treat both like
-- passwords, or use a server with access control and an access token:
-- uncomment the `auth` line and set the secret NTFY_TOKEN.
--
-- [gateway.connectors.ntfy]
-- settings = { topic = "<pushes go here>", reply_topic = "<you write here>" }
--
-- Pushes go to `settings.topic` (delivery target `ntfy:<topic>`). Write to
-- `settings.reply_topic` from the ntfy app to talk to tern; the first
-- message there must be the code from `tern gateway pair`. Approval and
-- outbox buttons publish your answer to the reply topic from the phone.

local SERVER = "https://ntfy.sh"
-- The title an answer button publishes with, so a tap isn't read as text.
local ACTION = "tern-action"

local function check(r)
  if not r.ok then
    error("ntfy answered " .. tostring(r.status) .. ": " .. string.sub(r.body or "", 1, 200))
  end
  return r
end

local function title(opts)
  local kind = opts.kind
  if kind == "approval" then return "tern needs your OK" end
  if kind == "outbox" then return "tern: waiting for you" end
  if kind == "notice" then return "tern needs something" end
  return "tern"
end

local function publish(ctx, to, text, opts, actions)
  local urgent = opts.kind == "approval" or opts.kind == "outbox"
  local body = {
    topic = to,
    message = text,
    title = title(opts),
    priority = urgent and 4 or 3,
    tags = urgent and { "bell" } or nil,
    actions = actions,
  }
  check(ctx.http.post(SERVER .. "/", { json = body }))
end

return {
  name = "ntfy",
  description = "ntfy push notifications: send yourself a push, and the ntfy gateway",
  capabilities = {
    http = { "ntfy.sh" },
    -- auth = "NTFY_TOKEN",
    rate = "60/min",
  },
  tools = {
    push = {
      description = "Send a push notification to an ntfy topic",
      effect = "write",
      params = {
        type = "object",
        properties = {
          topic = { type = "string", description = "The topic" },
          message = { type = "string" },
          title = { type = "string" },
        },
        required = { "topic", "message" },
      },
      run = function(ctx, args)
        check(ctx.http.post(SERVER .. "/", { json = { topic = args.topic, message = args.message, title = args.title } }))
        return "sent"
      end,
    },
  },
  gateway = {
    kind = "ntfy",
    threads = false,
    send = function(ctx, to, text, opts)
      publish(ctx, to, text, opts, nil)
    end,
    -- Each button publishes its data to the reply topic, from the phone.
    buttons = function(ctx, to, text, buttons, opts)
      local reply = opts.settings and opts.settings.reply_topic
      if not reply then
        -- Nowhere to answer: the host's text fallback says how to reply.
        local lines = { text }
        for _, b in ipairs(buttons) do table.insert(lines, "• " .. b.label) end
        return publish(ctx, to, table.concat(lines, "\n"), opts, nil)
      end
      local actions = {}
      -- ntfy shows at most three buttons.
      for i, b in ipairs(buttons) do
        if i > 3 then break end
        table.insert(actions, {
          action = "http", label = b.label, url = SERVER .. "/" .. reply, method = "POST",
          headers = { Title = ACTION }, body = b.data, clear = true,
        })
      end
      if #buttons > 3 then
        text = text .. "\nMore choices: reply in the ntfy app."
      end
      publish(ctx, to, text, opts, actions)
    end,
    poll = function(ctx, cursor, settings)
      local reply = settings.reply_topic
      if not reply then return { messages = {}, cursor = cursor } end
      -- First poll: start from now, not from the topic's history.
      local since = cursor or tostring(os.time())
      local r = check(ctx.http.get(SERVER .. "/" .. reply .. "/json?poll=1&since=" .. since))
      local messages, last = {}, cursor
      for line in string.gmatch(r.body or "", "[^\n]+") do
        local ok, m = pcall(json.decode, line)
        if ok and type(m) == "table" and m.event == "message" then
          last = m.id
          -- Replies belong to the push topic's chat: answers go back there.
          local msg = { id = m.id, chat = settings.topic or reply }
          if m.title == ACTION then msg.action = m.message else msg.text = m.message end
          table.insert(messages, msg)
        end
      end
      return { messages = messages, cursor = last or since }
    end,
  },
}

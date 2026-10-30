-- t3rex: T3 Code actions for Rex. Installed by `t3rex install`, loaded from init.lua with
-- require("t3rex"). Each action acts on the T3 thread whose window has focus.
local T3REX = "@T3REX@"

local function t3rex(ctx, args)
  local argv = { T3REX }
  for _, a in ipairs(args) do table.insert(argv, a) end
  if ctx.block_id then
    table.insert(argv, "--block")
    table.insert(argv, ctx.block_id)
  end
  local r, err = rex.exec(argv, { timeout = 20 })
  if not r then error(err, 0) end
  if not r.ok then error((r.stderr ~= "" and r.stderr or r.stdout):gsub("^t3rex: ", ""), 0) end
  return r.stdout
end

local function thread_action(name, title, args, keywords)
  rex.action{
    name = name,
    title = title,
    category = "T3 Code",
    keywords = keywords,
    run = function(ctx) t3rex(ctx, args) end,
  }
end

thread_action("t3_settle", "Settle Thread", { "settle" }, "t3 done close archive inbox")
thread_action("t3_snooze_hour", "Snooze Thread for 1 Hour", { "snooze", "1h" }, "t3 later")
thread_action("t3_snooze_3h", "Snooze Thread for 3 Hours", { "snooze", "3h" }, "t3 later")
thread_action("t3_snooze_tomorrow", "Snooze Thread Until Tomorrow", { "snooze", "tomorrow" }, "t3 later")
thread_action("t3_pin", "Pin Thread", { "pin" }, "t3 keep")
thread_action("t3_unpin", "Unpin Thread", { "pin", "--off" }, "t3")
thread_action("t3_open", "Open Thread in T3 Code", { "open" }, "t3 app desktop")
thread_action("t3_archive", "Archive Thread", { "archive" }, "t3 delete")

rex.action{
  name = "t3_inbox",
  title = "Go to Thread That Needs You",
  category = "T3 Code",
  keywords = "t3 inbox next attention approval",
  run = function(ctx)
    local r = rex.exec({ T3REX, "inbox" }, { timeout = 10 })
    if not (r and r.ok) then error("t3rex isn't syncing", 0) end
    local target = rex.json.decode(r.stdout)
    if target and target.session_id then
      rex.client.queue("session.select", target)
    end
  end,
}

-- A new thread in the T3 Code session, from anywhere. In that session, New Tab does the same.
rex.action{
  name = "t3_new",
  title = "New Thread",
  category = "T3 Code",
  keywords = "t3 create start compose prompt",
  run = function(ctx)
    local r = rex.exec({ T3REX, "inbox" }, { timeout = 10 })
    if not (r and r.ok) then error("t3rex isn't syncing", 0) end
    local sid = rex.json.decode(r.stdout).session_id
    local w, err = rex.session.new_window{
      session_id = sid,
      window_label = "New Thread",
      focus = true,
      layout = { block = { flavor = "com.superlogical.terminal.shell", label = "New Thread",
        options = { shell = "none", login = false, command = { T3REX, "new" } } } },
    }
    if err then error(err, 0) end
    rex.session.disown_block{ session_id = sid }
    rex.client.queue("session.select", { session_id = sid, window_id = w.window_id })
  end,
}

thread_action("t3_model", "Change Model…", { "model" }, "t3 effort access provider reasoning")

rex.bind("ctrl+cmd+s", "t3_settle")
rex.bind("ctrl+cmd+z", "t3_snooze_hour")
rex.bind("ctrl+cmd+p", "t3_pin")
rex.bind("ctrl+cmd+o", "t3_open")
rex.bind("ctrl+cmd+i", "t3_inbox")
rex.bind("ctrl+cmd+n", "t3_new")
rex.bind("ctrl+cmd+m", "t3_model")

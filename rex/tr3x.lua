-- tr3x: T3 Code in Rex. Installed by `tr3x install`, loaded from init.lua with
-- require("tr3x"). Each action acts on the T3 thread whose window has focus.
local TR3X = "@TR3X@"
-- The Rex, T3 and state directory install found, for everything tr3x runs from here.
local ENV = { TR3X_REX = "@REX@", T3CODE_HOME = "@T3HOME@", TR3X_STATE_DIR = "@STATE@" }

-- Sync keeps the T3 Code session in step with T3, and starts as this file loads. A sync that's
-- already running holds a lock, so the one each reload starts leaves at once.
local function sync()
  rex.exec.start({ TR3X, "sync", "--log" }, { env = ENV, detach = true, stdout = false, stderr = false })
end
sync()

-- A pane runs with the Rex server's environment, so the paths above go on its command line.
local function pane(args)
  local argv = { TR3X }
  for k, v in pairs(ENV) do
    table.insert(argv, "--set-env")
    table.insert(argv, k .. "=" .. v)
  end
  for _, a in ipairs(args) do table.insert(argv, a) end
  return argv
end

-- What an action on a thread says when nothing names one: run from Shortcuts' Run Rex Action
-- or `rex do`, an action may have no block, or the last window may not be a thread's.
local NO_THREAD = "Go to a T3 thread first"

-- `tail` goes after the block, for text that could read as a flag.
local function tr3x(ctx, args, tail)
  local argv = { TR3X }
  for _, a in ipairs(args) do table.insert(argv, a) end
  if ctx.block_id then
    table.insert(argv, "--block")
    table.insert(argv, ctx.block_id)
  end
  for _, a in ipairs(tail or {}) do table.insert(argv, a) end
  local r, err = rex.exec(argv, { env = ENV, timeout = 20 })
  if not r then error(err, 0) end
  if not r.ok then error((r.stderr ~= "" and r.stderr or r.stdout):gsub("^tr3x: ", ""), 0) end
  return r.stdout
end

local function trim(text) return (text:gsub("^%s+", ""):gsub("%s+$", "")) end

-- Questions go in Rex's own palette (rex.ui, Rex API revision 17), on whichever device answers
-- first. A Rex app too old to show them answers "unsupported", and the action opens tr3x's own
-- floating window instead.
local function native() return rex.ui ~= nil and rex.ui.pick ~= nil end

-- What a question that went unanswered means for the action: nothing, unless Rex couldn't ask
-- (an app too old for prompts, or a prompt that failed), and tr3x's own window should.
local function unasked(why)
  if why == "unsupported" or why == "failed" then return true end
  if why ~= "cancelled" and why ~= "timeout" then rex.log("warn", "tr3x: the palette didn't ask: " .. tostring(why)) end
  return false
end

-- The rex.ui items `tr3x <args>` prints.
local function items(args)
  return rex.json.decode(tr3x({}, args))
end

-- A floating layer over the thread, running tr3x.
local function layer(ctx, label, bounds, argv)
  local _, err = rex.session.new_layer{ session_id = ctx.session_id, bounds = bounds, focus = true,
    layout = { block = { flavor = "com.superlogical.terminal.shell", label = label,
      options = { shell = "none", login = false, command = pane(argv) } } } }
  rex.session.disown_block{ session_id = ctx.session_id }
  if err then error(err, 0) end
end

-- The thread actions that ask in the palette, by name. Each returns false when Rex couldn't
-- ask, and its floating window opens instead.
local ASK = {}

ASK.t3_rename = function(ctx)
  local current = trim(tr3x(ctx, { "title" }))
  local title, why = rex.ui.input{ title = "Rename Thread", value = current, placeholder = "Title", confirm = "Rename" }
  if title == nil then return not unasked(why) end
  title = trim(title)
  if title ~= "" and title ~= current then tr3x(ctx, { "rename" }, { "--", title }) end
  return true
end

ASK.t3_delete = function(ctx)
  local current = trim(tr3x(ctx, { "title" }))
  if current == "" then current = "this thread" end
  local choice, why = rex.ui.pick{ title = "Delete Thread", confirm = "Choose", items = {
    { id = "keep", title = "Keep it" },
    { id = "delete", title = "Delete " .. current, subtitle = "For good, in T3 too" },
  } }
  if choice == nil then return not unasked(why) end
  if choice == "delete" then tr3x(ctx, { "delete", "--yes" }) end
  return true
end

-- An action on the focused thread. `opts.client` passes the client that ran it, for an action
-- that goes to a tab it makes.
local function thread_action(name, title, args, keywords, opts)
  rex.action{
    name = name,
    title = title,
    category = "T3 Code",
    keywords = keywords,
    run = function(ctx)
      if not ctx.block_id then error(NO_THREAD, 0) end
      local argv = {}
      for _, a in ipairs(args) do table.insert(argv, a) end
      if opts and opts.client then
        table.insert(argv, "--client")
        table.insert(argv, ctx.client_id)
      end
      tr3x(ctx, argv)
    end,
  }
end

-- An action that asks something first: in the palette, else in a floating layer over the thread.
local function layer_action(name, title, args, keywords)
  rex.action{
    name = name,
    title = title,
    category = "T3 Code",
    keywords = keywords,
    run = function(ctx)
      if not ctx.block_id then error(NO_THREAD, 0) end
      if ASK[name] and native() and ASK[name](ctx) then return end
      local argv = {}
      for _, a in ipairs(args) do table.insert(argv, a) end
      table.insert(argv, "--block")
      table.insert(argv, ctx.block_id)
      layer(ctx, title, { 0.2, 0.3, 0.6, 0.3 }, argv)
    end,
  }
end

-- The thread actions, from tr3x's list of them (actions.rs), each also a `/` command in Reply.
@ACTIONS@
-- Snooze for a time from a list, or one typed in: 45m, 2h, 2d.
local SNOOZE = {
  { id = "1h", title = "1 hour", icon = "clock" },
  { id = "3h", title = "3 hours", icon = "clock" },
  { id = "tomorrow", title = "Tomorrow", detail = "9am", icon = "clock" },
  { id = "7d", title = "1 week", icon = "clock" },
}
rex.action{
  name = "t3_snooze",
  title = "Snooze Thread…",
  category = "T3 Code",
  keywords = "t3 later remind hide",
  run = function(ctx)
    if not ctx.block_id then error(NO_THREAD, 0) end
    if not native() then error("Update Rex to choose a time here; Reply's /snooze takes one", 0) end
    local title, failed = "Snooze Thread", nil
    for _ = 1, 3 do
      local choice, why = rex.ui.pick{ title = title, placeholder = "Or type a time, such as 45m or 2d",
        confirm = "Snooze", items = SNOOZE, allow_other = true }
      if choice == nil then
        if unasked(why) then error("Update Rex to choose a time here; Reply's /snooze takes one", 0) end
        return
      end
      if type(choice) == "table" then choice = trim(choice.text) end
      local r = rex.exec({ TR3X, "snooze", "--block", ctx.block_id, "--", choice }, { env = ENV, timeout = 20 })
      if r and r.ok then return end
      failed = r and trim((r.stderr ~= "" and r.stderr or r.stdout):gsub("^tr3x: ", "")) or "tr3x didn't run"
      title = "Snooze Thread: can't read “" .. choice .. "”"
    end
    error(failed, 0)
  end,
}
rex.action{
  name = "t3_inbox",
  title = "Go to Thread That Needs You",
  category = "T3 Code",
  keywords = "t3 inbox next attention approval",
  run = function(ctx)
    local r = rex.exec({ TR3X, "inbox" }, { env = ENV, timeout = 10 })
    if not (r and r.ok) then sync(); error("tr3x wasn't syncing; starting it", 0) end
    local target = rex.json.decode(r.stdout)
    if target and target.session_id then
      rex.client.queue("session.select", target)
    end
  end,
}

-- Answers an approval from anywhere: the focused thread's, else the thread ⌃⌘I goes to.
local function answer_action(name, title, decision, keywords)
  rex.action{
    name = name,
    title = title,
    category = "T3 Code",
    keywords = keywords,
    run = function(ctx) tr3x(ctx, { "answer", decision }) end,
  }
end
answer_action("t3_approve", "Approve", "approve", "t3 allow accept yes permission run")
answer_action("t3_approve_always", "Approve Always", "always", "t3 allow accept session permission")
answer_action("t3_decline", "Decline", "decline", "t3 deny reject no permission")

-- What a thread waits on, answered in the palette: an approval, or each of an agent's questions
-- with its options, on whichever device answers first. Returns "answered", "skipped" (passed
-- over, with `opts.skip`) or nil to stop. A secret goes to its tab: the palette can't hide what's
-- typed into it.
local GO, SKIP = "tr3x:go", "tr3x:skip"
local APPROVAL_ICON = { Run = "terminal", Edit = "document", Read = "document" }

local function waiting(ctx, skip)
  local args = { "waiting" }
  for _, k in ipairs(skip or {}) do table.insert(args, "--skip"); table.insert(args, k) end
  local w = rex.json.decode(tr3x(ctx, args))
  if type(w) ~= "table" then return nil end
  return w
end

local function go(w)
  if type(w.tab) == "table" and w.tab.session_id then rex.client.queue("session.select", w.tab) end
end

local function answer_in_palette(w, opts)
  if w.kind == "secret" then go(w); return nil end
  local extra = { { id = GO, title = "Go to Thread", subtitle = w.title, icon = "link" } }
  if opts and opts.skip then table.insert(extra, { id = SKIP, title = "Skip", subtitle = "Answer it later", icon = "clock" }) end
  local function chosen(id)
    if id == GO then go(w); return nil end
    if id == SKIP then return "skipped" end
  end
  if w.kind == "approval" then
    local items = {
      { id = "approve", title = "Approve", subtitle = w.what, icon = APPROVAL_ICON[w.verb] or "gear" },
      { id = "always", title = "Approve Always", subtitle = "For the rest of this session" },
      { id = "decline", title = "Decline" },
    }
    for _, e in ipairs(extra) do table.insert(items, e) end
    local choice = rex.ui.pick{ title = w.title, placeholder = w.verb .. " " .. w.what .. "?", items = items }
    if choice == nil then return nil end
    if choice == GO or choice == SKIP then return chosen(choice) end
    tr3x({}, { "respond", "--thread", w.thread, "--request", w.request, "--decision", choice })
    return "answered"
  end
  local answers = {}
  for _, q in ipairs(w.questions or {}) do
    local title = q.header or w.title
    local answer
    if #(q.options or {}) == 0 then
      answer = rex.ui.input{ title = title, placeholder = q.question, confirm = "Answer" }
      if answer == nil then return nil end
    else
      local items = {}
      for _, o in ipairs(q.options) do table.insert(items, { id = o.id, title = o.title, subtitle = o.subtitle }) end
      if not q.multiple then for _, e in ipairs(extra) do table.insert(items, e) end end
      answer = rex.ui.pick{ title = title, placeholder = q.question, items = items,
        multiple = q.multiple, allow_other = not q.multiple, confirm = q.multiple and "Answer" or nil }
      if answer == nil then return nil end
      if answer == GO or answer == SKIP then return chosen(answer) end
      if type(answer) == "table" and answer.text then answer = answer.text end
    end
    answers[q.id] = answer
  end
  tr3x({}, { "respond", "--thread", w.thread, "--request", w.request, "--answers", rex.json.encode(answers) })
  return "answered"
end

local NOTHING = "Nothing is waiting on you"

-- The focused thread's approval or question, else the next thread's, in the palette. Where Rex
-- can't ask there, it goes to that thread's tab instead.
rex.action{
  name = "t3_answer",
  title = "Answer…",
  category = "T3 Code",
  keywords = "t3 approve decline allow question reply respond permission needs waiting",
  run = function(ctx)
    local w = waiting(ctx)
    if not w then error(NOTHING, 0) end
    if not native() then return go(w) end
    answer_in_palette(w)
  end,
}

-- Everything waiting on you, one after another, on any machine: answer each, skip it, or stop.
rex.action{
  name = "t3_answer_next",
  title = "Answer Next…",
  category = "T3 Code",
  keywords = "t3 approve decline question inbox all every waiting queue triage",
  run = function(ctx)
    if not native() then error("Update Rex to answer here; ⌃⌘I goes to what needs you", 0) end
    local done = {}
    for n = 1, 25 do
      local w = waiting(ctx, done)
      if not w then
        if n == 1 then error(NOTHING, 0) end
        return
      end
      local result = answer_in_palette(w, { skip = true })
      if result == nil then return end
      table.insert(done, w.thread)
    end
  end,
}

-- From a subagent up to the thread it works for, and from a thread down to its subagent. Each
-- goes on the screen that asked, so a phone and a Mac can be in different places.
local function hop(name, title, command, keywords)
  rex.action{
    name = name,
    title = title,
    category = "T3 Code",
    keywords = keywords,
    run = function(ctx)
      if not ctx.block_id then error(NO_THREAD, 0) end
      local target = rex.json.decode(tr3x(ctx, { command }))
      if target and target.session_id then
        rex.client.queue("session.select", target)
      end
    end,
  }
end
hop("t3_parent", "Go to Thread This Subagent Works For", "parent", "t3 subagent parent up back")
hop("t3_child", "Go to Subagent", "child", "t3 subagent child down")

-- A new thread's window in the T3 Code session, running the composer with `args`.
local function compose(args)
  local r = rex.exec({ TR3X, "inbox" }, { env = ENV, timeout = 10 })
  if not (r and r.ok) then sync(); error("tr3x wasn't syncing; starting it", 0) end
  local sid = rex.json.decode(r.stdout).home
  local argv = { "new" }
  for _, a in ipairs(args) do table.insert(argv, a) end
  local w, err = rex.session.new_window{
    session_id = sid,
    window_label = "New Thread",
    focus = true,
    layout = { block = { flavor = "com.superlogical.terminal.shell", label = "New Thread",
      options = { shell = "none", login = false, command = pane(argv) } } },
  }
  if err then error(err, 0) end
  rex.session.disown_block{ session_id = sid }
  rex.client.queue("session.select", { session_id = sid, window_id = w.window_id })
end

-- A new thread in the T3 Code session, from anywhere. In that session, New Tab does the same.
rex.action{
  name = "t3_new",
  title = "New Thread",
  category = "T3 Code",
  keywords = "t3 create start compose prompt",
  run = function() compose({}) end,
}

-- A project, then where in it: its folder, a worktree it has, or a new one. The composer opens
-- on that, for the first message.
rex.action{
  name = "t3_project",
  title = "Open Project…",
  category = "T3 Code",
  keywords = "t3 project folder repo worktree branch new thread start",
  run = function()
    if not native() then error("Update Rex to pick here; New Thread (⌃⌘N) picks a project too", 0) end
    local projects = items({ "projects" })
    local project, why = rex.ui.pick{ title = "Open Project", placeholder = "Filter projects",
      confirm = "Next", items = projects }
    if project == nil then
      if unasked(why) then error("Update Rex to pick here; New Thread (⌃⌘N) picks a project too", 0) end
      return
    end
    local name = project
    for _, p in ipairs(projects) do if p.id == project then name = p.title end end
    local places = items({ "workspaces", project })
    local place = places[1].id
    if #places > 1 then
      place, why = rex.ui.pick{ title = name, placeholder = "Filter worktrees", confirm = "New Thread", items = places }
      if place == nil then return end
    end
    local env, id = project:match("^([^/]+)/(.+)$")
    compose({ "--env", env, "--project", id, "--workspace", place })
  end,
}

-- Every thread on every machine, settled, snoozed and archived too: Enter brings one back to
-- its tab. In the palette where Rex can ask; else in tr3x's floating list, where Option+Enter
-- opens it in T3 (Rex keeps Cmd+Enter).
rex.action{
  name = "t3_find",
  title = "Find Thread…",
  category = "T3 Code",
  keywords = "t3 search go switch jump settled snoozed archived closed reopen restore history",
  run = function(ctx)
    if native() then
      local key, why = rex.ui.pick{ title = "Find Thread", placeholder = "Filter threads", confirm = "Go",
        items = items({ "find", "--items" }) }
      if key ~= nil then
        local target = rex.json.decode(tr3x({}, { "find", "--go", key }))
        if target and target.session_id then rex.client.queue("session.select", target) end
        return
      end
      if not unasked(why) then return end
    end
    layer(ctx, "Find Thread", { 0.2, 0.2, 0.6, 0.6 }, { "find", "--client", ctx.client_id })
  end,
}

-- Undo for a tab closed by accident: the thread settled last comes back.
rex.action{
  name = "t3_reopen_settled",
  title = "Reopen Last Settled",
  category = "T3 Code",
  keywords = "t3 undo closed tab restore unsettle",
  run = function(ctx) tr3x({}, { "reopen", "--client", ctx.client_id }) end,
}

-- Six keys, for what you reach for from anywhere (⌃⌘T is bound with Fit to This Device).
-- Everything else is in the palette under T3 Code and in Reply's `/` commands; bind any of it
-- yourself in init.lua.
rex.bind("ctrl+cmd+i", "t3_inbox")
rex.bind("ctrl+cmd+n", "t3_new")
rex.bind("ctrl+cmd+o", "t3_project")
rex.bind("ctrl+cmd+a", "t3_answer")
rex.bind("ctrl+cmd+[", "t3_parent")
rex.bind("ctrl+cmd+]", "t3_child")

-- The client that ran an action, from the server's list: its principal says where it is.
local function caller(ctx)
  local r = rex.call("client.list", {})
  for _, c in ipairs((r and r.clients) or {}) do
    if c.client_id == ctx.client_id then return c end
  end
end

-- Opens T3 Code on this Mac. From a laptop or phone attached over the tailnet, opening it here
-- would open it on the wrong screen, so the thread's title, a link that device opens, does it.
rex.action{
  name = "t3_open",
  title = "Open Thread in T3 Code",
  category = "T3 Code",
  keywords = "t3 app desktop",
  run = function(ctx)
    local c = caller(ctx)
    if c and c.principal and c.principal.transport ~= "unix" then
      error("Click the thread's title to open it on this device", 0)
    end
    if not ctx.block_id then error(NO_THREAD, 0) end
    tr3x(ctx, { "open" })
  end,
}

-- The device that runs this takes the T3 Code session's panes, so they size to its screen and
-- follow its appearance. The others show that size until one of them takes the panes back.
-- Rex's app takes a pane only when it switches to that tab, so coming back to a device that
-- still shows the thread leaves it sized by the last one: ⌃⌘T fixes that.
rex.action{
  name = "t3_fit",
  title = "Fit to This Device",
  category = "T3 Code",
  keywords = "t3 size resize screen phone ipad laptop layout own",
  run = function(ctx)
    local r = rex.exec({ TR3X, "inbox" }, { env = ENV, timeout = 10 })
    if not (r and r.ok) then error("tr3x isn't syncing", 0) end
    for _, sid in ipairs(rex.json.decode(r.stdout).sessions or {}) do
      local _, err = rex.session.own_block{ session_id = sid, mode = "take" }
      if err then error(err, 0) end
    end
  end,
}
rex.bind("ctrl+cmd+t", "t3_fit")

-- Pairs another machine that runs T3, in a floating window: over SSH, or with a pasted link.
rex.action{
  name = "t3_pair",
  title = "Pair Another Machine…",
  category = "T3 Code",
  keywords = "t3 machine device computer environment link connect ssh join",
  run = function(ctx)
    local _, err = rex.session.new_layer{ session_id = ctx.session_id, bounds = { 0.15, 0.2, 0.7, 0.5 }, focus = true,
      layout = { block = { flavor = "com.superlogical.terminal.shell", label = "Pair",
        options = { shell = "none", login = false, command = pane({ "pair", "--wait" }) } } } }
    rex.session.disown_block{ session_id = ctx.session_id }
    if err then error(err, 0) end
  end,
}

-- What's new for tr3x, T3 on each machine and the Rex apps here, in a floating window that asks
-- before it installs anything.
rex.action{
  name = "t3_update",
  title = "Update tr3x, T3 and Rex…",
  category = "T3 Code",
  keywords = "t3 update upgrade version release new install",
  run = function(ctx)
    local _, err = rex.session.new_layer{ session_id = ctx.session_id, bounds = { 0.15, 0.2, 0.7, 0.5 }, focus = true,
      layout = { block = { flavor = "com.superlogical.terminal.shell", label = "Updates",
        options = { shell = "none", login = false, command = pane({ "update", "--wait" }) } } } }
    rex.session.disown_block{ session_id = ctx.session_id }
    if err then error(err, 0) end
  end,
}

-- One T3 Code session for every machine, or a session for each.
local function sessions_action(name, title, split)
  rex.action{
    name = name,
    title = title,
    category = "T3 Code",
    keywords = "t3 machine device sessions split merge",
    run = function(ctx) tr3x({}, { "sessions", split }) end,
  }
end
sessions_action("t3_sessions_per_machine", "Sessions: One per Machine", "per-machine")
sessions_action("t3_sessions_merged", "Sessions: One for All Machines", "merged")

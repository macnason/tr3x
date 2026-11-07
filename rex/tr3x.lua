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

local function tr3x(ctx, args)
  local argv = { TR3X }
  for _, a in ipairs(args) do table.insert(argv, a) end
  if ctx.block_id then
    table.insert(argv, "--block")
    table.insert(argv, ctx.block_id)
  end
  local r, err = rex.exec(argv, { env = ENV, timeout = 20 })
  if not r then error(err, 0) end
  if not r.ok then error((r.stderr ~= "" and r.stderr or r.stdout):gsub("^tr3x: ", ""), 0) end
  return r.stdout
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

-- An action that asks something first, in a floating layer over the thread.
local function layer_action(name, title, args, keywords)
  rex.action{
    name = name,
    title = title,
    category = "T3 Code",
    keywords = keywords,
    run = function(ctx)
      if not ctx.block_id then error(NO_THREAD, 0) end
      local argv = {}
      for _, a in ipairs(args) do table.insert(argv, a) end
      table.insert(argv, "--block")
      table.insert(argv, ctx.block_id)
      local _, err = rex.session.new_layer{ session_id = ctx.session_id, bounds = { 0.2, 0.3, 0.6, 0.3 }, focus = true,
        layout = { block = { flavor = "com.superlogical.terminal.shell", label = title,
          options = { shell = "none", login = false, command = pane(argv) } } } }
      rex.session.disown_block{ session_id = ctx.session_id }
      if err then error(err, 0) end
    end,
  }
end

-- The thread actions, from tr3x's list of them (actions.rs), each also a `/` command in Reply.
@ACTIONS@
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

-- A new thread in the T3 Code session, from anywhere. In that session, New Tab does the same.
rex.action{
  name = "t3_new",
  title = "New Thread",
  category = "T3 Code",
  keywords = "t3 create start compose prompt",
  run = function(ctx)
    local r = rex.exec({ TR3X, "inbox" }, { env = ENV, timeout = 10 })
    if not (r and r.ok) then sync(); error("tr3x wasn't syncing; starting it", 0) end
    local sid = rex.json.decode(r.stdout).home
    local w, err = rex.session.new_window{
      session_id = sid,
      window_label = "New Thread",
      focus = true,
      layout = { block = { flavor = "com.superlogical.terminal.shell", label = "New Thread",
        options = { shell = "none", login = false, command = pane({ "new" }) } } },
    }
    if err then error(err, 0) end
    rex.session.disown_block{ session_id = sid }
    rex.client.queue("session.select", { session_id = sid, window_id = w.window_id })
  end,
}

-- Every thread on every machine, settled, snoozed and archived too, in a floating list: Enter
-- brings one back to its tab, Option+Enter opens it in T3 (Rex keeps Cmd+Enter).
rex.action{
  name = "t3_find",
  title = "Find Thread…",
  category = "T3 Code",
  keywords = "t3 search settled snoozed archived closed reopen restore history",
  run = function(ctx)
    local _, err = rex.session.new_layer{ session_id = ctx.session_id, bounds = { 0.2, 0.2, 0.6, 0.6 }, focus = true,
      layout = { block = { flavor = "com.superlogical.terminal.shell", label = "Find Thread",
        options = { shell = "none", login = false, command = pane({ "find", "--client", ctx.client_id }) } } } }
    rex.session.disown_block{ session_id = ctx.session_id }
    if err then error(err, 0) end
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

-- Four keys, for what you reach for from anywhere. Everything else is in the palette under T3 Code
-- and in Reply's `/` commands; bind any of it yourself in init.lua.
rex.bind("ctrl+cmd+i", "t3_inbox")
rex.bind("ctrl+cmd+n", "t3_new")
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

-- nerdy-deadbeef · Lua macros & rules (READ-ONLY TEMPLATE)
--
-- API
--   deadbeef.dispatch(name, args)   run any dispatcher (same as `dbctl dispatch`)
--   deadbeef.keyword(key, value)    set an option at runtime (same as `dbctl keyword`)
--   deadbeef.option(key)            read an option, e.g. deadbeef.option("general:gaps_in")
--   deadbeef.clients()              list of { hwnd, exe, class, title, floating, monitor }
--   deadbeef.log(msg)               write to %APPDATA%\deadbeef\deadbeef.log
--   deadbeef.on(event, fn)          events: "windowopen", "focus"
--   deadbeef.launcher               contents of launcher.ini
--
-- A "windowopen" handler may return a rule table:
--   { float = true, workspace = 3, silent = true, monitor = 1, center = true,
--     size = {800, 600}, opacity = 0.9, pin = true, noborder = true, ignore = true }

-- bind = SUPER, G, lua, toggle_gaps
local saved_gaps = nil
function toggle_gaps()
  if saved_gaps then
    deadbeef.keyword("general:gaps_in", saved_gaps[1])
    deadbeef.keyword("general:gaps_out", saved_gaps[2])
    saved_gaps = nil
  else
    saved_gaps = { deadbeef.option("general:gaps_in"), deadbeef.option("general:gaps_out") }
    deadbeef.keyword("general:gaps_in", 0)
    deadbeef.keyword("general:gaps_out", 0)
  end
end

-- Float small settings-style windows that don't announce themselves as dialogs.
deadbeef.on("windowopen", function(c)
  if c.exe == "SystemSettings.exe" then
    return { float = true, center = true, size = { "60%", "70%" } }
  end
end)

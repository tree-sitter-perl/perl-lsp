-- E2E: goto-def on a function-like macro call returns the `#define` FIRST
-- (the macro identity), then SEES THROUGH to the wrapped function as a
-- second, ranked location. `docs/adr/macro-handling.md` (the identity/nav lane).
vim.opt.rtp:prepend(".")
local t   = require("test.runner")
local lsp = require("test.lsp")
local b   = require("test.buf")
local buf = lsp.open_and_attach("test_files/cpp/macro_calls.c")

t.test("macro-wrapper call wrap() -> #define then realFunc", function()
  local N = "wrap-seethrough"
  local l, c = b.find_pos(buf, "wrap(5)")
  local defs = lsp.def_lines(buf, l, c)
  local ok = t.eq(N, b.find_line(buf, "#define wrap"), defs[1], "first result is the #define")
  ok = t.contains(N, defs, b.find_line(buf, "int realFunc"), "sees through to realFunc") and ok
  if ok then t.pass(N) end
end)

t.test("thread-context wrapper newThing() -> #define then Perl_newThing", function()
  local N = "newThing-seethrough"
  local l, c = b.find_pos(buf, "newThing(7)")
  local defs = lsp.def_lines(buf, l, c)
  local ok = t.eq(N, b.find_line(buf, "#define newThing"), defs[1], "first result is the #define")
  ok = t.contains(N, defs, b.find_line(buf, "int Perl_newThing"), "sees through to Perl_newThing") and ok
  if ok then t.pass(N) end
end)
t.finish()

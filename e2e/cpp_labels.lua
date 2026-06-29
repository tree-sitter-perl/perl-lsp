-- E2E: C goto-label navigation (`goto done` -> `done:`).
vim.opt.rtp:prepend(".")
local t   = require("test.runner")
local lsp = require("test.lsp")
local b   = require("test.buf")
local buf = lsp.open_and_attach("test_files/cpp/labels.c")

t.test("goto LABEL resolves to its label def", function()
  local N = "goto-label"
  local l, c = b.find_pos(buf, "goto done")
  local def = lsp.def_line(buf, l, c + 5)
  if t.eq(N, b.find_line(buf, "^done:"), def, "goto done -> done:") then t.pass(N) end
end)

t.test("labels are hidden from the outline", function()
  local N = "label-outline"
  local names = lsp.symbol_names(buf)
  local set = {}; for _, n in ipairs(names) do set[n] = true end
  if t.ok(N, set["f"] and not set["done"], "f shown, done hidden: " .. vim.inspect(names)) then t.pass(N) end
end)
t.done()

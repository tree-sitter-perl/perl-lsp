-- C++/Python member completion via sentinel reparse generalizes: `w.` → Widget's methods.
vim.opt.rtp:prepend(".")
local t = require("test.runner")
local lsp = require("test.lsp")
local b = require("test.buf")

local buf = lsp.open_and_attach("test_files/python/members.py")

t.test("member completion: w. offers Widget methods", function()
  local N = "py member completion"
  local line, col = b.find_pos(buf, "w.")
  if not t.ok(N, line, "no 'w.' line") then return end
  local labels = lsp.completion_labels(buf, line, col + 2)
  if not t.contains(N, labels, "grow", "member labels") then return end
  if t.contains(N, labels, "shrink", "member labels") then t.pass(N) end
end)

local lbuf = lsp.open_and_attach("test_files/python/labels.py")

t.test("label completion: a call's argument offers the callee's keywords", function()
  local N = "py label completion"
  local probe = 'connect(host="h", '
  b.invalidate() -- the line cache holds the first buffer's lines
  local line, col = b.find_pos(lbuf, probe)
  if not t.ok(N, line, "no call line") then return end
  local labels = lsp.completion_labels(lbuf, line, col + #probe)
  if not t.contains(N, labels, "timeout=", "labels") then return end
  if not t.contains(N, labels, "retry=", "labels") then return end
  for _, l in ipairs(labels) do
    if l == "host=" then t.fail(N, "host= is already passed") return end
  end
  t.pass(N)
end)

t.finish()

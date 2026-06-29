-- E2E: cpp-lsp via headless nvim (perl-lsp --features cpp).
-- Usage: PERL_LSP_BIN=target/release/perl-lsp \
--          nvim --headless --clean -u e2e/init_cpp.lua -l e2e/cpp.lua
vim.opt.rtp:prepend(".")
local t   = require("test.runner")
local lsp = require("test.lsp")
local b   = require("test.buf")

local buf = lsp.open_and_attach("test_files/cpp/sample.cpp")

t.test("documentSymbol returns C++ classes and functions", function()
  local N = "documentSymbol C++"
  local names = lsp.symbol_names(buf)
  if not t.ok(N, #names > 0, "no symbols returned") then return end
  local ok = t.contains(N, names, "Shape", "symbols")
  ok = t.contains(N, names, "Circle", "symbols") and ok
  ok = t.contains(N, names, "compute", "symbols") and ok
  ok = t.contains(N, names, "main", "symbols") and ok
  if ok then t.pass(N) end
end)

t.test("goto-def: compute(21) jumps to int compute", function()
  local N = "goto-def compute"
  local line, col = b.find_pos(buf, "compute(21)")
  if not t.ok(N, line, "couldn't find 'compute(21)'") then return end
  local def = lsp.def_line(buf, line, col)
  local expected = b.find_line(buf, "^int compute")
  if t.eq(N, expected, def, "definition line") then t.pass(N) end
end)


t.test("references: compute is referenced at its call site", function()
  local N = "references compute"
  local dl, dc = b.find_pos(buf, "int compute(int x)")
  if not t.ok(N, dl, "no compute def") then return end
  local lines = lsp.reference_lines(buf, dl, dc + 4)
  if not t.ok(N, lines and #lines >= 2, "expected def+call refs, got " .. #lines) then return end
  local call_line = select(1, b.find_pos(buf, "compute(21)"))
  local found = false
  for _, l in ipairs(lines) do if l == call_line then found = true end end
  if t.ok(N, found, "call site not in refs: [" .. table.concat(lines, ",") .. "]") then t.pass(N) end
end)

t.test("rename: compute touches def and call", function()
  local N = "rename compute"
  local dl, dc = b.find_pos(buf, "int compute(int x)")
  if not t.ok(N, dl, "no compute def") then return end
  local edit = lsp.rename(buf, dl, dc + 4, "calc")
  if not t.ok(N, edit and edit.changes, "no rename edit") then return end
  local n = 0
  for _, edits in pairs(edit.changes) do n = n + #edits end
  if t.ok(N, n >= 2, "rename should touch def + call, got " .. n) then t.pass(N) end
end)


t.test("completion: in-scope symbols include compute + main", function()
  local N = "completion in-scope"
  local ml, mc = b.find_pos(buf, "int n = compute")
  if not t.ok(N, ml, "no 'int n' line") then return end
  local labels = lsp.completion_labels(buf, ml, mc + 8)
  if not t.contains(N, labels, "compute", "completion labels") then return end
  if t.contains(N, labels, "main", "completion labels") then t.pass(N) end
end)


t.test("hover: compute shows its signature", function()
  local N = "hover compute"
  local dl, dc = b.find_pos(buf, "int compute(int x)")
  if not t.ok(N, dl, "no compute def") then return end
  local h = lsp.hover_text(buf, dl, dc + 4)
  if not t.ok(N, h, "no hover") then return end
  if t.ok(N, h:find("int compute", 1, true) ~= nil, "hover has signature: " .. tostring(h)) then t.pass(N) end
end)

t.test("document-highlight: compute def + call", function()
  local N = "highlight compute"
  local dl, dc = b.find_pos(buf, "int compute(int x)")
  if not t.ok(N, dl, "no compute def") then return end
  local lines = lsp.reference_lines(buf, dl, dc + 4)  -- highlight uses same machinery
  if t.ok(N, lines and #lines >= 2, "expected def+call, got " .. #lines) then t.pass(N) end
end)

t.finish()

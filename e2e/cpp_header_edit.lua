-- E2E: H1 cache lifecycle — an edit to a SAVED header becomes visible to
-- its includer mid-session, no restart: the new macro + function resolve
-- cross-file at fresh positions (frozen pack index / never-evicted macro
-- caches were the arc-review H1 finding).
-- Usage: PERL_LSP_BIN=target/release/perl-lsp \
--          nvim --headless --clean -u e2e/init_cpp.lua -l e2e/cpp_header_edit.lua
vim.opt.rtp:prepend(".")
local t   = require("test.runner")
local lsp = require("test.lsp")

-- Fresh two-file workspace in a temp dir (never dirties the repo). A `.git`
-- marker makes it the LSP root so the pack index covers it.
local ws = vim.fn.tempname()
vim.fn.mkdir(ws .. "/.git", "p")
local hdr = ws .. "/hdr.h"
local mainc = ws .. "/main.c"
local hdr_v1 = { "#define LIMIT 5", "int helper(void);" }
local hdr_v2 = {
  "#define LIMIT 5",
  "int helper(void);",
  "#define LIMIT2 7",
  "int helper2(void);",
}
vim.fn.writefile(hdr_v1, hdr)
vim.fn.writefile({
  '#include "hdr.h"',
  "int use_it(void) {",
  "    int a = LIMIT;",
  "    int b = LIMIT2;",
  "    helper();",
  "    helper2();",
  "    return a + b;",
}, mainc)

local buf = lsp.open_and_attach(mainc)

local function def_loc(line, col) return lsp.def_location(buf, line, col) end

-- Wait for the lazy pack index + background gather: LIMIT → hdr.h.
local ready = false
for _ = 1, 60 do
  local loc = def_loc(2, 13) -- inside LIMIT on `int a = LIMIT;`
  if loc and loc.uri:find("hdr%.h$") then
    ready = true
    break
  end
  vim.wait(500)
end

t.test("baseline: LIMIT resolves cross-file into hdr.h", function()
  local N = "baseline LIMIT cross-file"
  if t.ok(N, ready, "cross-file def never warmed") then t.pass(N) end
end)

t.test("saved header edit is visible to the includer without restart", function()
  local N = "H1 header edit visible"
  -- Edit + SAVE the header through the editor (didOpen/didChange/didSave).
  vim.cmd("edit " .. vim.fn.fnameescape(hdr))
  local hbuf = vim.api.nvim_get_current_buf()
  vim.api.nvim_buf_set_lines(hbuf, 0, -1, false, hdr_v2)
  vim.cmd("write")
  -- Poll: the includer must resolve the NEW macro + function mid-session.
  local got_macro, got_fn
  for _ = 1, 60 do
    got_macro = def_loc(3, 13) -- inside LIMIT2 on `int b = LIMIT2;`
    got_fn = def_loc(5, 5)     -- inside helper2 on `helper2();`
    if got_macro and got_fn then break end
    vim.wait(500)
  end
  if not t.ok(N, got_macro, "LIMIT2 still unresolvable after header save") then return end
  if not t.ok(N, got_fn, "helper2 still unresolvable after header save") then return end
  local ok = t.ok(N, got_macro.uri:find("hdr%.h$") ~= nil,
    "LIMIT2 resolved into " .. tostring(got_macro.uri))
  -- Fresh POSITION, not a stale-index row: v2 declares helper2 on row 3.
  ok = t.eq(N, 3, got_fn.line, "helper2 def row") and ok
  if ok then t.pass(N) end
end)

t.finish()

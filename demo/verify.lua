-- Headless verification that the demo fixture exercises the features we
-- want to record. Run from repo root:
--   nvim --headless --clean -u test_nvim_init.lua -l demo/verify.lua
package.path = package.path .. ";./lua/?.lua"
local lsp = require("test.lsp")

local function has(list, want)
  for _, v in ipairs(list) do if v == want then return true end end
  return false
end

local buf = lsp.open_and_attach("demo/app.pl")

-- Poll for cross-file readiness: completion on $acct-> should surface Account's
-- methods once the workspace index + enrichment land.
local labels = {}
for _ = 1, 60 do
  labels = lsp.completion_labels(buf, 7, 7) -- after `$acct->`
  if has(labels, "deposit") and has(labels, "describe") then break end
  vim.wait(250)
end

io.write("completion@$acct->: " .. table.concat(labels, ", ") .. "\n")
io.write("  deposit?  " .. tostring(has(labels, "deposit")) .. "\n")
io.write("  describe? " .. tostring(has(labels, "describe")) .. "\n")
io.write("  owner?    " .. tostring(has(labels, "owner")) .. "\n")
io.write("  balance?  " .. tostring(has(labels, "balance")) .. "\n")

local hov = lsp.hover_text(buf, 5, 4) -- $acct in `my $acct = ...`
io.write("hover@$acct: " .. tostring(hov and hov:gsub("\n", " ⏎ ")) .. "\n")

local def = lsp.def_location(buf, 7, 9) -- `deposit` in `$acct->deposit`
io.write("goto-def deposit: " .. tostring(def and (def.uri .. " L" .. def.line)) .. "\n")

local edit = lsp.rename(buf, 4, 12, "Ledger") -- `Account` in `use Account;`? test class rename later
io.write("rename returned edit: " .. tostring(edit ~= nil) .. "\n")

vim.cmd("qa!")

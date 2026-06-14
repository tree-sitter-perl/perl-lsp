-- E2E: branded edges — two named Mojo::Lite apps in one workspace must
-- not leak helpers across each other. Both apps' helpers bridge to the
-- same app-surface class; per-file branding keeps them separate.
-- See docs/adr/branded-edges.md.
--
-- Usage: nvim --headless --clean -u test_nvim_init.lua -l test_e2e_branded_apps.lua

vim.opt.rtp:prepend(".")

local t   = require("test.runner")
local lsp = require("test.lsp")
local b   = require("test.buf")

local function has(labels, name)
  for _, l in ipairs(labels) do if l == name then return true end end
  return false
end

-- Completion on `$c->` (the controller, an app-surface consumer) lists
-- helpers reachable through the app-surface bridge.
local function helpers_on(buf)
  local line, col = b.find_pos(buf, "$c->render")
  if not line then return {} end
  return lsp.completion_labels(buf, line, col + 4) -- after "$c->"
end

-- Poll until the cross-file workspace index settles: `buf`'s OWN helper
-- `own` must surface on `$c->`.
local function wait_ready(buf, own)
  for _ = 1, 60 do
    if has(helpers_on(buf), own) then return true end
    vim.wait(250)
  end
  io.write("\27[33mWARN: helper completion not ready within 15s for " .. own .. "\27[0m\n")
  return false
end

local two = lsp.open_and_attach("test_files/branded_app_two.pl")
-- Wait on a CROSS-FILE global helper, not app two's own: it proves the
-- workspace index is warm (so app one IS indexed and alpha_only's absence
-- below is the brand filter, not a cold index). Environment-independent —
-- the helper is plugin-synthesized, no CPAN module needed.
wait_ready(two, "shared_global_helper")

t.test("branded edges: app two sees its own + shared global helpers", function()
  local N = "branded edges: app two sees its own + shared global helpers"
  local labels = helpers_on(two)
  -- own helper still visible (branding must not hide it), and the
  -- unbranded shared plugin's helper rides along (brands are additive).
  local ok = t.contains(N, labels, "beta_only", "own helper")
  ok = t.contains(N, labels, "shared_global_helper", "global helper") and ok
  if ok then t.pass(N) end
end)

t.test("branded edges: app one's helper does NOT leak into app two", function()
  local N = "branded edges: app one's helper does NOT leak into app two"
  local labels = helpers_on(two)
  -- app one is a named .pl in the same workspace, and the index is warm
  -- (shared_global_helper resolved cross-file) — so were app two NOT
  -- branded, alpha_only would surface here (cf. the unbranded integration
  -- test). Its absence is the brand filter doing its job.
  if t.ok(N, not has(labels, "alpha_only"),
      "alpha_only must NOT leak into app two; got: [" .. table.concat(labels, ", ") .. "]") then
    t.pass(N)
  end
end)

t.finish()

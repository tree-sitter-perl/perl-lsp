-- nvim config for cpp-lsp (perl-lsp built --features cpp). Drives C AND C++.
-- Shares all keymaps / completion / sig-help DX with the Perl config via
-- dev_lsp.lua. Usage: ./dev.sh <file.c/.h/.cpp>  (or -u this for the e2e).
local here = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h")
dofile(here .. "/dev_lsp.lua")({
  filetypes = { "c", "cpp" },
  root_markers = { ".git", "CMakeLists.txt", "Makefile", "configure", "compile_commands.json" },
  attach_message = "cpp-lsp attached! gd=def gr=refs K=hover <leader>rn=rename <leader>o=symbols <leader>f=format",
})

-- Semantic token colors — base groups apply to c/cpp (Perl uses .perl-suffixed).
vim.highlight.priorities.semantic_tokens = 200
vim.api.nvim_set_hl(0, "@lsp.type.class", { fg = "#e0af68", bold = true })   -- gold — types/classes
vim.api.nvim_set_hl(0, "@lsp.type.function", { fg = "#7aa2f7" })             -- blue — functions
vim.api.nvim_set_hl(0, "@lsp.type.method", { fg = "#7dcfff" })               -- cyan — methods
vim.api.nvim_set_hl(0, "@lsp.type.variable", { fg = "#61afef" })             -- blue — variables
vim.api.nvim_set_hl(0, "@lsp.type.property", { fg = "#73daca" })             -- teal — fields
vim.api.nvim_set_hl(0, "@lsp.type.macro", { fg = "#bb9af7", bold = true })   -- purple — macros

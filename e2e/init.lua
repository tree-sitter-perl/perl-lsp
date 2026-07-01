-- Throwaway nvim config for testing perl-lsp (Perl).
-- Usage: nvim --clean -u e2e/init.lua test_files/sample.pl
--   PERL_LSP_DEBUG=1 ... → debug log at /tmp/perl-lsp.log

local here = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h")
dofile(here .. "/dev_lsp.lua")({
  filetypes = { "perl" },
  root_markers = { ".git", "Makefile", "cpanfile", "Makefile.PL", "Build.PL" },
  attach_message = "perl-lsp attached! gd=def gi=impl gr=refs K=hover <leader>rn=rename <leader>o=symbols <leader>f=format",
})

-- Semantic token highlight groups for perl-lsp — loud and distinct for QA.
vim.highlight.priorities.semantic_tokens = 200

vim.api.nvim_set_hl(0, "@lsp.type.variable.perl", { fg = "#61afef" })         -- blue — scalars/arrays/hashes
vim.api.nvim_set_hl(0, "@lsp.type.parameter.perl", { fg = "#ff9e64", bold = true }) -- orange bold — sub params
vim.api.nvim_set_hl(0, "@lsp.type.function.perl", { fg = "#7aa2f7" })         -- bright blue — function calls
vim.api.nvim_set_hl(0, "@lsp.type.method.perl", { fg = "#7dcfff" })           -- cyan — method calls
vim.api.nvim_set_hl(0, "@lsp.type.macro.perl", { fg = "#bb9af7", bold = true }) -- purple bold — has/with/extends
vim.api.nvim_set_hl(0, "@lsp.type.property.perl", { fg = "#73daca" })         -- teal — hash keys
vim.api.nvim_set_hl(0, "@lsp.type.namespace.perl", { fg = "#e0af68", bold = true }) -- gold bold — Foo::Bar
-- $self/$class: force hot pink even when base syntax tries to override (e.g. inside `my()`)
vim.api.nvim_set_hl(0, "@lsp.type.keyword.perl", { fg = "#ff007c", bold = true })
vim.api.nvim_set_hl(0, "@lsp.typemod.keyword.declaration.perl", { fg = "#ff007c", bold = true, underline = true })
vim.api.nvim_set_hl(0, "@lsp.type.enumMember.perl", { fg = "#ff9e64", italic = true }) -- orange italic — constants
vim.api.nvim_set_hl(0, "@lsp.type.regexp.perl", { fg = "#9ece6a" })           -- green — regex

vim.api.nvim_set_hl(0, "@lsp.mod.declaration.perl", { bold = true, underline = true })
vim.api.nvim_set_hl(0, "@lsp.mod.modification.perl", { fg = "#f7768e" })  -- red for writes
vim.api.nvim_set_hl(0, "@lsp.mod.readonly.perl", { italic = true })
vim.api.nvim_set_hl(0, "@lsp.mod.defaultLibrary.perl", { italic = true })
vim.api.nvim_set_hl(0, "@lsp.mod.deprecated.perl", { strikethrough = true })

vim.api.nvim_set_hl(0, "@lsp.mod.scalar.perl", { fg = "#61afef" })  -- blue
vim.api.nvim_set_hl(0, "@lsp.mod.array.perl", { fg = "#c678dd" })   -- purple
vim.api.nvim_set_hl(0, "@lsp.mod.hash.perl", { fg = "#e5c07b" })    -- gold

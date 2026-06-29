-- nvim config for cpp-lsp (perl-lsp built --features cpp). Drives C AND C++.
-- Usage: PERL_LSP_BIN=target/release/perl-lsp nvim --clean -u e2e/init_cpp.lua <file>
--    or: ./dev.sh <file.c/.h/.cpp>
vim.opt.number = true
vim.opt.signcolumn = "yes"
vim.opt.updatetime = 300
vim.opt.completeopt = { "menuone", "noselect", "popup" }
vim.opt.pumheight = 15

local lsp_bin = vim.env.PERL_LSP_BIN
  and vim.fn.fnamemodify(vim.env.PERL_LSP_BIN, ":p")
  or vim.fn.fnamemodify("target/release/perl-lsp", ":p")

vim.lsp.config["perl-lsp"] = {
  cmd = { lsp_bin },
  filetypes = { "c", "cpp" },
  root_markers = { ".git", "CMakeLists.txt", "Makefile", "Makefile.PL", "configure" },
}
vim.lsp.enable("perl-lsp")
vim.api.nvim_create_autocmd("LspAttach", {
  callback = function() print("cpp-lsp attached!") end,
})

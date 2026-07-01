-- Shared dev-nvim LSP setup for perl-lsp (used by init.lua and init_cpp.lua).
-- One place for the binary resolution, debug wiring, vim.lsp.config, and the
-- LspAttach keymaps/completion/sig-help — so every language gets the same DX.
--
-- Usage (from a `-u` init script):
--   local here = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h")
--   dofile(here .. "/dev_lsp.lua")({
--     filetypes = { "perl" },
--     root_markers = { ".git", "cpanfile" },
--     attach_message = "perl-lsp attached! ...",
--   })

return function(opts)
  opts = opts or {}

  -- Minimal editor settings (completion popup, signs, inlay room)
  vim.opt.number = true
  vim.opt.signcolumn = "yes"
  vim.opt.updatetime = 300
  vim.opt.completeopt = { "menuone", "noselect", "popup" }
  vim.opt.pumheight = 15

  -- Window navigation (muscle memory): Ctrl-h/j/k/l
  vim.keymap.set("n", "<C-h>", "<C-w>h")
  vim.keymap.set("n", "<C-j>", "<C-w>j")
  vim.keymap.set("n", "<C-k>", "<C-w>k")
  vim.keymap.set("n", "<C-l>", "<C-w>l")

  -- Built binary. Override with PERL_LSP_BIN for comparison runs.
  local lsp_bin = vim.env.PERL_LSP_BIN
    and vim.fn.fnamemodify(vim.env.PERL_LSP_BIN, ":p")
    or vim.fn.fnamemodify("target/release/perl-lsp", ":p")

  -- Debug mode: PERL_LSP_DEBUG=1 → RUST_LOG to /tmp/perl-lsp.log
  local debug_mode = vim.env.PERL_LSP_DEBUG == "1"
  local log_file = "/tmp/perl-lsp.log"
  local cmd
  if debug_mode then
    cmd = {
      "sh", "-c",
      "RUST_LOG=perl_lsp=debug exec " .. vim.fn.shellescape(lsp_bin) .. " 2>>" .. log_file,
    }
  else
    cmd = { lsp_bin }
  end

  vim.lsp.config["perl-lsp"] = {
    cmd = cmd,
    filetypes = opts.filetypes or { "perl" },
    root_markers = opts.root_markers or { ".git" },
  }
  vim.lsp.enable("perl-lsp")

  -- Keybindings + DX, set up on LspAttach (shared across languages)
  vim.api.nvim_create_autocmd("LspAttach", {
    callback = function(args)
      local buf = args.buf
      local client_id = args.data.client_id
      local kopts = { buffer = buf }

      -- Built-in LSP completion (nvim 0.11+), autotrigger on server triggers
      vim.lsp.completion.enable(true, client_id, buf, { autotrigger = true })
      vim.lsp.inlay_hint.enable(true, { bufnr = buf })

      -- Navigation
      vim.keymap.set("n", "gd", vim.lsp.buf.definition, kopts)
      vim.keymap.set("n", "gi", vim.lsp.buf.implementation, kopts)
      vim.keymap.set("n", "gr", vim.lsp.buf.references, kopts)
      vim.keymap.set("n", "K", vim.lsp.buf.hover, kopts)

      -- Rename / outline
      vim.keymap.set("n", "<leader>rn", vim.lsp.buf.rename, kopts)
      vim.keymap.set("n", "<leader>o", vim.lsp.buf.document_symbol, kopts)

      -- Document highlight: symbol under cursor
      vim.api.nvim_create_autocmd({ "CursorHold", "CursorHoldI" }, {
        buffer = buf,
        callback = vim.lsp.buf.document_highlight,
      })
      vim.api.nvim_create_autocmd("CursorMoved", {
        buffer = buf,
        callback = vim.lsp.buf.clear_references,
      })

      -- Smart expand/shrink selection (selectionRange): + parent, - child
      local sel_stack = {}
      local function clamp(lnum, col)
        local last_line = vim.api.nvim_buf_line_count(buf)
        lnum = math.max(1, math.min(lnum, last_line))
        local line_text = vim.api.nvim_buf_get_lines(buf, lnum - 1, lnum, false)[1] or ""
        col = math.max(0, math.min(col, math.max(0, #line_text - 1)))
        return lnum, col
      end
      local function flatten_sr(node)
        local ranges = {}
        while node do
          table.insert(ranges, node.range)
          node = node.parent
        end
        return ranges
      end
      local function set_visual(r)
        local sl, sc = clamp(r.start.line + 1, r.start.character)
        local el, ec = clamp(r["end"].line + 1, math.max(0, r["end"].character - 1))
        vim.cmd("normal! \\<Esc>")
        vim.api.nvim_win_set_cursor(0, { sl, sc })
        vim.cmd("normal! v")
        vim.api.nvim_win_set_cursor(0, { el, ec })
      end
      vim.keymap.set({ "n", "v" }, "+", function()
        local sr = vim.lsp.buf_request_sync(buf, "textDocument/selectionRange", {
          textDocument = vim.lsp.util.make_text_document_params(buf),
          positions = { vim.lsp.util.make_position_params(0, "utf-16").position },
        }, 1000)
        if not sr then return end
        for _, res in pairs(sr) do
          if res.result and res.result[1] then
            local ranges = flatten_sr(res.result[1])
            local idx = #sel_stack + 1
            if idx <= #ranges then
              sel_stack[idx] = ranges[idx]
              set_visual(ranges[idx])
            end
            return
          end
        end
      end, kopts)
      vim.keymap.set("v", "-", function()
        if #sel_stack > 1 then
          table.remove(sel_stack)
          set_visual(sel_stack[#sel_stack])
        elseif #sel_stack == 1 then
          sel_stack = {}
          vim.cmd("normal! \\<Esc>")
        end
      end, kopts)
      vim.api.nvim_create_autocmd("ModeChanged", {
        pattern = "v:n",
        callback = function() sel_stack = {} end,
      })

      -- Signature help: trigger on ( and , ; re-trigger inside parens
      vim.keymap.set("i", "<C-s>", vim.lsp.buf.signature_help, kopts)
      vim.api.nvim_create_autocmd("TextChangedI", {
        buffer = buf,
        callback = function()
          local col = vim.fn.col(".") - 1
          if col <= 0 then return end
          local line = vim.api.nvim_get_current_line()
          local before = line:sub(1, col)
          local char = before:sub(-1)
          if char == "(" or char == "," then
            vim.schedule(function()
              if vim.fn.mode() == "i" then vim.lsp.buf.signature_help() end
            end)
            return
          end
          local opens = select(2, before:gsub("%(", ""))
          local closes = select(2, before:gsub("%)", ""))
          if opens > closes then
            vim.schedule(function()
              if vim.fn.mode() == "i" then vim.lsp.buf.signature_help() end
            end)
          end
        end,
      })

      -- Format + diagnostics nav
      vim.keymap.set("n", "<leader>f", vim.lsp.buf.format, kopts)
      vim.keymap.set("n", "[d", vim.diagnostic.goto_prev, kopts)
      vim.keymap.set("n", "]d", vim.diagnostic.goto_next, kopts)

      -- Manual completion (C-Space) + bareword autotrigger
      vim.keymap.set("i", "<C-Space>", function() vim.lsp.completion.get() end, kopts)
      vim.api.nvim_create_autocmd("InsertCharPre", {
        buffer = buf,
        callback = function()
          if vim.fn.pumvisible() == 1 then return end
          local char = vim.v.char
          if not char:match("[%w_]") then return end
          local col = vim.fn.col(".") - 1
          if col <= 0 then return end
          local line = vim.api.nvim_get_current_line()
          local word = line:sub(1, col):match("[%a_][%w_:]*$")
          if not word then return end
          vim.schedule(function()
            if vim.fn.mode() == "i" and vim.fn.pumvisible() == 0 then
              vim.lsp.completion.get()
            end
          end)
        end,
      })

      print(opts.attach_message or "perl-lsp attached! gd=def gi=impl gr=refs K=hover <leader>rn=rename <leader>o=symbols <leader>f=format")
    end,
  })
end

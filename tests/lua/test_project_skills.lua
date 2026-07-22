return function(t)
  local config = require("loopbiotic.config")
  local context = require("loopbiotic.context")
  local skills = require("loopbiotic.skills")
  local state = require("loopbiotic.state")
  local surfaces = require("loopbiotic.surfaces")
  local ui = require("loopbiotic.ui")

  t.test("workspace symbol queries keep the concrete short subject", function()
    t.eq(
      context.workspace_queries("How would you improve the tree types and why?", 3),
      { "types", "tree" }
    )
  end)

  local function fixture()
    local root = vim.fn.tempname()
    vim.fn.mkdir(root .. "/apps/web-angular", "p")
    vim.fn.mkdir(root .. "/apps/editor-react", "p")
    vim.fn.mkdir(root .. "/apps/api-rust", "p")
    vim.fn.mkdir(root .. "/crates/graph-model", "p")
    vim.fn.mkdir(root .. "/deploy/docker", "p")
    vim.fn.writefile({ "Repository rules" }, root .. "/AGENTS.md")
    vim.fn.writefile({ "Project overview" }, root .. "/README.md")
    vim.fn.writefile({ "ignore" }, root .. "/notes.txt")
    vim.fn.writefile({
      vim.json.encode({
        dependencies = {
          ["@angular/core"] = "22.0.6",
          react = "18.3.1",
          ["@excalidraw/excalidraw"] = "0.18.1",
        },
        devDependencies = { typescript = "~6.0.0", nx = "23.1.0" },
      }),
    }, root .. "/package.json")
    vim.fn.writefile({
      vim.json.encode({
        version = "5",
        specifiers = {
          ["npm:@angular/core@22.0.6"] = "22.0.6_rxjs@7.8.2",
          ["npm:react@18.3.1"] = "18.3.1",
          ["npm:@excalidraw/excalidraw@0.18.1"] = "0.18.1_react@18.3.1",
          ["npm:nx@23.1.0"] = "23.1.0",
          ["npm:typescript@6.0"] = "6.0.3",
        },
      }),
    }, root .. "/deno.lock")
    vim.fn.writefile(
      { vim.json.encode({ tasks = { check = "nx run-many -t build", dev = "nx serve web-angular" } }) },
      root .. "/deno.json"
    )
    vim.fn.writefile({ "{}" }, root .. "/nx.json")
    vim.fn.writefile({ "FROM denoland/deno:2.9.0" }, root .. "/deploy/docker/web.Dockerfile")
    vim.fn.writefile({
      vim.json.encode({
        name = "web-angular",
        sourceRoot = "apps/web-angular/src",
        projectType = "application",
        targets = { build = { executor = "@angular/build:application" } },
      }),
    }, root .. "/apps/web-angular/project.json")
    vim.fn.writefile({
      vim.json.encode({
        name = "editor-react",
        sourceRoot = "apps/editor-react/src",
        projectType = "library",
        implicitDependencies = { "web-angular" },
      }),
    }, root .. "/apps/editor-react/project.json")
    vim.fn.writefile({
      "[workspace]",
      'members = ["apps/api-rust", "crates/graph-model"]',
      "[workspace.package]",
      'edition = "2024"',
      "[workspace.dependencies]",
      'axum = "0.8.9"',
      'sqlx = "0.9.0"',
      'tokio = "1.49.0"',
    }, root .. "/Cargo.toml")
    vim.fn.writefile({
      "services:",
      "  postgres:",
      "    image: postgres:17-alpine",
      "  garage:",
      "    image: dxflrs/garage:v2.3.0",
    }, root .. "/docker-compose.yml")
    return root
  end

  t.test("Neovim supplies bounded LSP facts without profiling the project", function()
    local root = fixture()
    local previous_get_clients = vim.lsp.get_clients
    vim.lsp.get_clients = function()
      return {
        {
          name = "angularls",
          config = { root_dir = root .. "/apps/web-angular" },
          server_info = { version = "22" },
          server_capabilities = {
            definitionProvider = true,
            diagnosticProvider = true,
          },
        },
      }
    end

    local signals = context.project_signals(0, root)

    t.eq(#signals.lsp_clients, 1)
    t.eq(signals.lsp_clients[1].name, "angularls")
    t.eq(signals.lsp_clients[1].root, "apps/web-angular")
    t.eq(signals.lsp_clients[1].capabilities[1], "definition")
    t.eq(signals.lsp_clients[1].capabilities[2], "diagnostics")

    vim.lsp.get_clients = previous_get_clients
    vim.fn.delete(root, "rf")
  end)

  t.test("root Markdown skills are inert, session-selected, and content-addressed", function()
    local root = fixture()
    local previous = vim.deepcopy(config.values.skills)
    config.values.skills = {
      autoload = { "AGENTS.md" },
      discover_root_markdown = true,
      max_file_bytes = 65536,
      picker_height = 10,
    }
    state.session_id = nil
    skills.reset()
    skills.prepare(root)

    local items = skills.items()
    t.eq(#items, 2)
    t.eq(items[1].path, "AGENTS.md")
    t.eq(items[1].auto, true)
    t.eq(skills.selected("AGENTS.md"), true)
    t.eq(skills.toggle("AGENTS.md"), false, "config autoload is locked")
    t.eq(skills.toggle("README.md"), true)
    t.eq(skills.summary(), "Skills AGENTS.md · README.md")

    local snapshot = skills.snapshot()
    t.eq(#snapshot, 2)
    t.eq(snapshot[1].provenance, "config")
    t.eq(snapshot[2].content, "Project overview")
    t.eq(type(snapshot[2].sha256), "string")
    t.eq(#snapshot[2].sha256 > 0, true)

    skills.reset()
    state.session_id = nil
    config.values.skills = previous
    vim.fn.delete(root, "rf")
  end)

  t.test("Markdown symlinks cannot escape the workspace", function()
    local root = fixture()
    local outside = vim.fn.tempname() .. ".md"
    vim.fn.writefile({ "outside instructions" }, outside)
    vim.uv.fs_symlink(outside, root .. "/OUTSIDE.md")
    local previous = vim.deepcopy(config.values.skills)
    config.values.skills = {
      autoload = { "AGENTS.md" },
      discover_root_markdown = true,
      max_file_bytes = 65536,
      picker_height = 10,
    }

    skills.reset()
    skills.prepare(root)
    for _, item in ipairs(skills.items()) do
      t.eq(item.path == "OUTSIDE.md", false)
    end

    skills.reset()
    config.values.skills = previous
    vim.fn.delete(root, "rf")
    vim.fn.delete(outside)
  end)

  t.test("Skills picker is a subordinate Frame above PromptWindow", function()
    state.reset()
    surfaces.open_prompt({
      row = 14,
      col = 4,
      outer_width = 60,
      outer_height = 10,
      inner_width = 52,
      inner_height = 6,
      padding_x = 4,
      padding_y = 2,
      title = " Prompt ",
      footer = " footer ",
    })
    local _, picker = surfaces.open_prompt_picker({ "[ ] README.md", "[a] AGENTS.md" }, {
      title = " Skills ",
      footer = " Enter apply ",
    })
    local prompt_config = vim.api.nvim_win_get_config(state.surfaces.prompt.frame_win)
    local picker_config = vim.api.nvim_win_get_config(picker)

    t.eq(ui.number(picker_config.row) < ui.number(prompt_config.row), true)
    t.eq(surfaces.prompt_open(), true)
    surfaces.close_prompt({ focus_agent = false })
    t.eq(vim.api.nvim_win_is_valid(picker), false, "picker closes with PromptWindow")
  end)

  t.test("open_picker binds toggle, apply and cancel; cancel restores the selection", function()
    local root = fixture()
    local previous = vim.deepcopy(config.values.skills)
    config.values.skills = {
      autoload = { "AGENTS.md" },
      discover_root_markdown = true,
      max_file_bytes = 65536,
      picker_height = 10,
    }
    state.reset()
    state.session_id = nil
    skills.reset()
    skills.prepare(root)
    surfaces.open_prompt({
      row = 14,
      col = 4,
      outer_width = 60,
      outer_height = 10,
      inner_width = 52,
      inner_height = 6,
      padding_x = 4,
      padding_y = 2,
      title = " Prompt ",
      footer = " footer ",
    })

    skills.open_picker()
    local buf = state.surfaces.prompt.picker_buf
    t.eq(type(buf), "number", "picker opened")

    local function bound(lhs)
      local wanted = vim.api.nvim_replace_termcodes(lhs, true, true, true)
      for _, map in ipairs(vim.api.nvim_buf_get_keymap(buf, "n")) do
        if vim.api.nvim_replace_termcodes(map.lhs, true, true, true) == wanted then
          return map.callback
        end
      end
    end
    t.eq(type(bound("<Space>")), "function", "Space toggles")
    t.eq(type(bound("<CR>")), "function", "Enter applies")
    t.eq(type(bound("q")), "function", "q cancels")
    t.eq(type(bound("<Esc>")), "function", "Esc cancels")

    t.eq(skills.toggle("README.md"), true)
    t.eq(skills.selected("README.md"), true)
    bound("q")()
    t.eq(skills.selected("README.md"), false, "cancel restores the pre-picker selection")
    t.eq(state.surfaces.prompt.picker_win, nil, "cancel closes the picker")

    surfaces.close_prompt({ focus_agent = false })
    skills.reset()
    config.values.skills = previous
    vim.fn.delete(root, "rf")
  end)

  t.test("keymaps.skills defaults to the PromptWindow multiselect", function()
    t.eq(config.values.keymaps.skills, "<C-g>")
  end)
end

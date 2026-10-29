(() => {
  const tabButtons = [...document.querySelectorAll("[data-terminal-tab]")];

  const activateTab = (button) => {
    for (const candidate of tabButtons) {
      const selected = candidate === button;
      candidate.setAttribute("aria-selected", String(selected));
      candidate.tabIndex = selected ? 0 : -1;
      const panel = document.getElementById(candidate.dataset.terminalTab);
      if (panel) panel.hidden = !selected;
    }
  };

  tabButtons.forEach((button, index) => {
    button.addEventListener("click", () => activateTab(button));
    button.addEventListener("keydown", (event) => {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      event.preventDefault();
      const direction = event.key === "ArrowRight" ? 1 : -1;
      const next = tabButtons[(index + direction + tabButtons.length) % tabButtons.length];
      activateTab(next);
      next.focus();
    });
  });

  const sourceButtons = [...document.querySelectorAll("[data-source-filter]")];
  const categoryButtons = [...document.querySelectorAll("[data-category-filter]")];
  const categoryClear = document.querySelector("[data-category-clear]");
  const commands = [...document.querySelectorAll("[data-command]")];
  const search = document.querySelector("[data-command-search]");
  const visibleCount = document.querySelector("[data-visible-tools]");
  const emptyState = document.querySelector("[data-tool-empty]");
  const activeCategories = new Set();
  let activeSource = "all";

  const syncSourceButtons = () => {
    sourceButtons.forEach((button) => {
      const active = button.dataset.sourceFilter === activeSource;
      button.classList.toggle("is-active", active);
      button.setAttribute("aria-pressed", String(active));
    });
  };

  const syncCategoryButtons = () => {
    categoryButtons.forEach((button) => {
      const active = activeCategories.has(button.dataset.categoryFilter);
      button.classList.toggle("is-active", active);
      button.setAttribute("aria-pressed", String(active));
    });
    const allActive = activeCategories.size === 0;
    categoryClear?.classList.toggle("is-active", allActive);
    categoryClear?.setAttribute("aria-pressed", String(allActive));
  };

  const updateToolFilter = () => {
    const query = search?.value.trim().toLowerCase() ?? "";
    let commandCount = 0;

    for (const command of commands) {
      const sourceMatches = activeSource === "all" || command.dataset.source === activeSource;
      const categoryMatches = activeCategories.size === 0
        || activeCategories.has(command.dataset.category);
      const queryMatches = !query || command.dataset.search.includes(query);
      const visible = sourceMatches && categoryMatches && queryMatches;
      command.hidden = !visible;
      if (visible) commandCount += 1;
    }

    if (visibleCount) visibleCount.textContent = String(commandCount);
    if (emptyState) emptyState.hidden = commandCount !== 0;
  };

  sourceButtons.forEach((button) => {
    button.addEventListener("click", () => {
      activeSource = button.dataset.sourceFilter ?? "all";
      syncSourceButtons();
      updateToolFilter();
    });
  });
  categoryButtons.forEach((button) => {
    button.addEventListener("click", () => {
      const category = button.dataset.categoryFilter;
      if (activeCategories.has(category)) activeCategories.delete(category);
      else activeCategories.add(category);
      syncCategoryButtons();
      updateToolFilter();
    });
  });
  categoryClear?.addEventListener("click", () => {
    activeCategories.clear();
    syncCategoryButtons();
    updateToolFilter();
  });
  search?.addEventListener("input", updateToolFilter);
  document.addEventListener("keydown", (event) => {
    if (event.key === "/" && !event.metaKey && !event.ctrlKey && !event.altKey) {
      const target = event.target;
      const editing = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target?.isContentEditable;
      if (!editing) {
        event.preventDefault();
        search?.focus();
      }
    } else if (event.key === "Escape") {
      activeSource = "all";
      activeCategories.clear();
      if (search) {
        search.value = "";
        search.blur();
      }
      syncSourceButtons();
      syncCategoryButtons();
      updateToolFilter();
    }
  });
  syncSourceButtons();
  syncCategoryButtons();
  updateToolFilter();
  const copyText = async (text) => {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return;
    }

    const textarea = document.createElement("textarea");
    textarea.value = text;
    textarea.setAttribute("readonly", "");
    textarea.style.position = "fixed";
    textarea.style.opacity = "0";
    document.body.appendChild(textarea);
    textarea.select();
    document.execCommand("copy");
    textarea.remove();
  };

  document.querySelectorAll("[data-copy-target]").forEach((button) => {
    button.addEventListener("click", async () => {
      const target = document.getElementById(button.dataset.copyTarget);
      if (!target) return;
      const originalLabel = button.textContent;
      try {
        await copyText(target.textContent.trim());
        button.textContent = "copied";
      } catch {
        button.textContent = "copy failed";
      }
      window.setTimeout(() => { button.textContent = originalLabel; }, 1600);
    });
  });
  const bootstrapData = document.getElementById("bootstrap-data");
  const bootstrapTarget = document.querySelector("[data-bootstrap-target]");
  const bootstrapCommand = document.querySelector("[data-bootstrap-command]");
  const bootstrapLabel = document.querySelector("[data-bootstrap-label]");
  const bootstrapMeta = document.querySelector("[data-bootstrap-meta]");
  const bootstrapRuntimes = [...document.querySelectorAll("[data-bootstrap-runtime]")];
  if (bootstrapData && bootstrapTarget && bootstrapCommand) {
    const data = JSON.parse(bootstrapData.textContent);
    const runtimeRunners = {
      python: "python3",
      node: "node",
      perl: "perl",
    };
    let activeRuntime = "shell";
    const formatBytes = (value) => `${value} B`;
    const shellQuote = (value) => `'${value.replaceAll("'", "'\"'\"'")}'`;
    const renderBootstrap = () => {
      const target = bootstrapTarget.value;
      const targetData = data.targets[target];
      const runtime = targetData.runtimes[activeRuntime];
      let command;
      let label;
      if (activeRuntime === "native") {
        command = [
          `curl -fL -o axe-bootstrap ${runtime.download_url}`,
          "chmod +x axe-bootstrap",
          "./axe-bootstrap",
        ].join("\n");
        label = "native ELF";
      } else {
        const content = atob(runtime.content_base64);
        if (activeRuntime === "shell") {
          command = `sh -c ${shellQuote(content)} --`;
        } else if (activeRuntime === "python") {
          command = `${runtimeRunners[activeRuntime]} -c ${shellQuote(`import base64;exec(compile(base64.b64decode(${JSON.stringify(runtime.content_base64)}),\"axe-bootstrap.py\",\"exec\"))`)}`;
        } else if (activeRuntime === "node") {
          command = `node -e ${shellQuote(`eval(Buffer.from(${JSON.stringify(runtime.content_base64)},\"base64\").toString())`)}`;
        } else {
          const hex = [...content].map((character) => character.charCodeAt(0).toString(16).padStart(2, "0")).join("");
          command = `perl -e ${shellQuote(`eval pack(\"H*\",\"${hex}\")`)}`;
        }
        label = runtime.label;
      }
      bootstrapCommand.textContent = command;
      if (bootstrapLabel) bootstrapLabel.textContent = label;
      if (bootstrapMeta) {
        bootstrapMeta.textContent = `${target} · ${formatBytes(runtime.size)} raw · ${formatBytes(runtime.base64_size)} base64 · ${formatBytes(runtime.hex_size)} hex`;
      }
      bootstrapRuntimes.forEach((button) => {
        button.setAttribute("aria-pressed", String(button.dataset.bootstrapRuntime === activeRuntime));
      });
    };
    bootstrapTarget.addEventListener("change", renderBootstrap);
    bootstrapRuntimes.forEach((button) => {
      button.addEventListener("click", () => {
        activeRuntime = button.dataset.bootstrapRuntime;
        renderBootstrap();
      });
    });
    renderBootstrap();
  }
})();

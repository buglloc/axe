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
})();

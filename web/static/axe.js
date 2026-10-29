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
  const resetButtons = [...document.querySelectorAll("[data-tool-reset]")];
  const sources = new Set(sourceButtons.map((button) => button.dataset.sourceFilter));
  const categories = new Set(categoryButtons.map((button) => button.dataset.categoryFilter));
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

  const currentQuery = () => search?.value.trim() ?? "";

  const writeUrl = () => {
    const params = new URLSearchParams();
    if (currentQuery()) params.set("q", currentQuery());
    if (activeSource !== "all") params.set("source", activeSource);
    if (activeCategories.size) params.set("category", [...activeCategories].join(","));
    const query = params.toString().replaceAll("%2C", ",");
    const url = `${window.location.pathname}${query ? `?${query}` : ""}${window.location.hash}`;
    window.history.replaceState(null, "", url);
  };

  const updateToolFilter = () => {
    const query = currentQuery().toLowerCase();
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

    const filtered = Boolean(query) || activeSource !== "all" || activeCategories.size > 0;
    if (visibleCount) visibleCount.textContent = String(commandCount);
    if (emptyState) emptyState.hidden = commandCount !== 0;
    resetButtons.forEach((button) => {
      if (!emptyState?.contains(button)) button.hidden = !filtered;
    });
    writeUrl();
  };

  const resetFilters = () => {
    activeSource = "all";
    activeCategories.clear();
    if (search) search.value = "";
    syncSourceButtons();
    syncCategoryButtons();
    updateToolFilter();
  };

  if (commands.length) {
    const params = new URLSearchParams(window.location.search);
    const source = params.get("source");
    if (source && sources.has(source)) activeSource = source;
    for (const category of (params.get("category") ?? "").split(",")) {
      if (categories.has(category)) activeCategories.add(category);
    }
    if (search) search.value = params.get("q") ?? "";

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
    resetButtons.forEach((button) => button.addEventListener("click", resetFilters));
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
        resetFilters();
        search?.blur();
      }
    });
    syncSourceButtons();
    syncCategoryButtons();
    updateToolFilter();
  }

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
  const bootstrapPanel = document.querySelector("[data-bootstrap-panel]");
  const bootstrapCommand = document.querySelector("[data-bootstrap-command]");
  if (bootstrapData && bootstrapPanel && bootstrapCommand) {
    const data = JSON.parse(bootstrapData.textContent);
    const targets = Object.keys(data.targets);
    const modeTabs = [...document.querySelectorAll("[data-bootstrap-mode]")];
    const optionButtons = [...document.querySelectorAll("[data-bootstrap-option]")];
    const modeControls = [...document.querySelectorAll("[data-bootstrap-for]")];
    const targetSelect = document.querySelector("[data-bootstrap-target]");
    const label = document.querySelector("[data-bootstrap-label]");
    const note = document.querySelector("[data-bootstrap-note]");
    const choice = { fetch: "curl", paste: "python", native: "printf" };
    let mode = "fetch";
    let renderToken = 0;
    // Releases before 0.12.1 publish the Native loader URL without its bytes.
    const hasLoaderBytes = targets.every((target) => data.targets[target].runtimes.native.content_base64);

    const bytesOf = (base64) => Uint8Array.from(atob(base64), (character) => character.charCodeAt(0));
    const textOf = (base64) => new TextDecoder().decode(bytesOf(base64));
    const hexOf = (bytes) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
    const isAlnum = (byte) => (byte >= 48 && byte <= 57) || (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122);
    const isOctalDigit = (byte) => byte >= 48 && byte <= 55;
    const quote = (text) => `'${text.replaceAll("'", "'\\''")}'`;
    const kilobytes = (count) => `${(count / 1024).toFixed(1)} KB`;

    // zlib format, which Python's zlib.decompress and Node's inflateSync read.
    const deflate = async (bytes) => {
      if (typeof CompressionStream !== "function") return null;
      const stream = new Blob([bytes]).stream().pipeThrough(new CompressionStream("deflate"));
      return new Uint8Array(await new Response(stream).arrayBuffer());
    };

    // POSIX printf escapes: alphanumerics stay literal, other bytes become the
    // shortest octal escape that the next character cannot extend.
    const printfFormat = (bytes) => {
      let format = "";
      bytes.forEach((byte, index) => {
        if (isAlnum(byte)) {
          format += String.fromCharCode(byte);
          return;
        }
        const next = bytes[index + 1];
        const digits = byte.toString(8);
        format += `\\${next !== undefined && isOctalDigit(next) ? digits.padStart(3, "0") : digits}`;
      });
      return format;
    };

    const fetchCommand = (tool) => {
      const url = data.targets[targets[0]].runtimes.shell.url;
      const download = tool === "curl" ? `curl -fo a ${url}` : `wget -O a ${url}`;
      return {
        command: `${download}\nsh a`,
        label: `${tool}, then sh`,
        note: "Saves the script as ./a, so run it from a writable directory. Needs HTTPS: without CA certificates, paste the Shell script instead.",
      };
    };

    const pasteCommand = async (runtimeName) => {
      const runtime = data.targets[targets[0]].runtimes[runtimeName];
      const bytes = bytesOf(runtime.content_base64);
      if (runtimeName === "shell") {
        return {
          command: `sh -c ${quote(textOf(runtime.content_base64))}`,
          label: "sh",
          note: "For hosts with curl, wget, Python, Perl, Node.js or bash but without working HTTPS. The script downloads AXE over plain HTTP; the pinned digest protects it.",
        };
      }
      if (runtimeName === "perl") {
        return {
          command: `perl -e 'eval pack"H*","${hexOf(bytes)}"or die$@'`,
          label: "perl",
          note: "Hex only, so perl-base without Compress::Zlib is enough.",
        };
      }
      const compressed = await deflate(bytes);
      if (runtimeName === "python") {
        return {
          command: compressed
            ? `python3 -c "import zlib;exec(zlib.decompress(bytes.fromhex('${hexOf(compressed)}')))"`
            : `python3 -c "exec(bytes.fromhex('${hexOf(bytes)}'))"`,
          label: "python3",
          note: "Python 3.6 or later.",
        };
      }
      return {
        command: compressed
          ? `node -e "eval(''+require('zlib').inflateSync(Buffer.from('${hexOf(compressed)}','hex')))"`
          : `node -e "eval(''+Buffer.from('${hexOf(bytes)}','hex'))"`,
        label: "node",
        note: "Any Node.js release with the zlib module.",
      };
    };

    const nativeCommand = (decoder) => {
      const target = targetSelect?.value ?? targets[0];
      const runtime = data.targets[target].runtimes.native;
      const url = runtime.stable_url;
      if (!runtime.content_base64) {
        return {
          command: `curl -fo b ${url}\nchmod +x b\n./b`,
          label: `${target}, download`,
          note: "This release does not include the loader bytes for pasting. Without a shell, download the file and copy it with kubectl cp or docker cp.",
        };
      }
      const bytes = bytesOf(runtime.content_base64);
      const write = decoder === "printf"
        ? `printf '${printfFormat(bytes)}' >b`
        : `base64 -d >b <<E\n${runtime.content_base64.match(/.{1,76}/g).join("\n")}\nE`;
      return {
        command: `${write}\nchmod +x b\n./b`,
        label: `${target}, ${decoder === "printf" ? "sh only" : "base64"}`,
        note: `Writes the ${runtime.size}-byte loader to ./b and runs it; the directory must allow execution. Without a shell, download ${url} and copy it with kubectl cp or docker cp.`,
      };
    };

    const render = async () => {
      const token = ++renderToken;
      const result = mode === "fetch"
        ? fetchCommand(choice.fetch)
        : mode === "paste" ? await pasteCommand(choice.paste) : nativeCommand(choice.native);
      if (token !== renderToken) return;
      bootstrapCommand.textContent = result.command;
      if (label) label.textContent = `${result.label} · ${result.command.length.toLocaleString("en-US")} characters`;
      if (note) note.textContent = result.note;
    };

    const sync = () => {
      modeTabs.forEach((tab) => {
        const selected = tab.dataset.bootstrapMode === mode;
        tab.setAttribute("aria-selected", String(selected));
        tab.tabIndex = selected ? 0 : -1;
        if (selected) bootstrapPanel.setAttribute("aria-labelledby", tab.id);
      });
      modeControls.forEach((control) => {
        const needsLoader = control.hasAttribute("data-bootstrap-needs-loader");
        control.hidden = control.dataset.bootstrapFor !== mode || (needsLoader && !hasLoaderBytes);
      });
      optionButtons.forEach((button) => {
        button.setAttribute("aria-pressed", String(choice[button.dataset.bootstrapOption] === button.dataset.value));
      });
      render();
    };

    modeTabs.forEach((tab, index) => {
      tab.addEventListener("click", () => {
        mode = tab.dataset.bootstrapMode;
        sync();
      });
      tab.addEventListener("keydown", (event) => {
        if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
        event.preventDefault();
        const next = modeTabs[(index + (event.key === "ArrowRight" ? 1 : -1) + modeTabs.length) % modeTabs.length];
        mode = next.dataset.bootstrapMode;
        sync();
        next.focus();
      });
    });
    optionButtons.forEach((button) => {
      button.addEventListener("click", () => {
        choice[button.dataset.bootstrapOption] = button.dataset.value;
        sync();
      });
    });
    targetSelect?.addEventListener("change", render);
    sync();
  }
})();

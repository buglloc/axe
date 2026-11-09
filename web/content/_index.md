+++
title = "AXE"
headline = "A rescue shell in a single executable"
intro = "AXE bundles an interactive shell, Unix tools, host diagnostics, a certificate-only SSH server and a verified on-demand tool store. Copy it to the host you need to inspect and run it there."

[[failures]]
condition = "No shell"
works = "[Brush](https://github.com/reubeno/brush) provides an interactive shell and runs scripts and pipelines."
[[failures]]
condition = "No coreutils or PATH"
works = "Bundled commands run without host coreutils or PATH."
[[failures]]
condition = "/proc unavailable"
works = "Bundled commands still start."
limits = "Process listings and other diagnostics that read procfs are incomplete or unavailable."
[[failures]]
condition = "Read-only or noexec filesystem"
works = "Bundled commands start from the original executable."
limits = "AXE cannot write a private executable copy, so child commands may fail if the original executable is also removed."
[[failures]]
condition = "Binary deleted or replaced"
works = "On Linux, a running AXE keeps launching bundled commands and SSH sessions from the original executable."
limits = "On macOS, only builtins, aliases, functions and `doctor` remain."
[[failures]]
condition = "Child processes cannot start"
works = "Shell builtins, aliases, functions and `doctor`."
limits = "Bundled commands do not fall back to host tools."
[[failures]]
condition = "No network"
works = "Bundled commands and Store tools already in the verified cache."
limits = "Store tools that are not cached are unavailable."
[[failures]]
condition = "No root"
works = "Commands and diagnostics that do not need privileges."
limits = "Operations that need privileges fail."

[sections]
downloadTitle = "Download"
downloadLead = "The Linux release is a static musl ELF executable. The target needs no loader, shell or coreutils. Other targets are in GitHub Releases."
toolsTitle = "Commands"
toolsLead = "Bundled commands are part of the executable. AXE Store downloads the other tools on first use and keeps them in a private, verified cache."
failuresTitle = "When parts of the host are broken"
failuresLead = "A failure in one part of the host does not stop the shell or turn off Store verification."
environmentTitle = "Host inspection"
environmentLead = "AXE reports virtualization, containers, sandboxing and restrictions that matter for recovery."
remoteTitle = "SSH through an outbound connection"
remoteLead = "Run the certificate-only SSH/SFTP server on the target. The standalone relay makes it reachable when the target can only open outbound TCP or QUIC connections."
+++

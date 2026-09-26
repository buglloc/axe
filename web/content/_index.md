+++
title = "AXE"
eyebrow = "Portable rescue shell"
headline = "When the system has nothing useful left."
tagline = "Assume nothing. Bring everything."
intro = "AXE is one executable with an interactive shell, Unix tools, host diagnostics, certificate-only SSH and a verified on-demand tool store. Run it on the host you need to inspect."
terminalLabel = "incident@unknown:/#"

[[survival]]
condition = "No shell"
response = "[Brush](https://github.com/reubeno/brush) is built in, with interactive Reedline and a minimal backend for scripts and pipes."
code = "SHELL"
outcome = "interactive shell"
[[survival]]
condition = "No coreutils"
response = "Bundled applets run without PATH. If the BusyBox-style PATH bridge cannot be published, direct dispatch still works."
code = "PATH"
outcome = "local toolbox"
[[survival]]
condition = "No procfs"
response = "Missing or masked /proc removes one execution route. On Linux, AXE retains its executable descriptor so children can start without procfs."
code = "PROC"
outcome = "fd-backed self-exec"
[[survival]]
condition = "Broken filesystem"
response = "AXE tries descriptor exec, then a probed private relay and an inode-checked path. Single-file Store tools can run from a sealed memfd."
code = "EXEC"
outcome = "checked fallback"
[[survival]]
condition = "Binary deleted"
response = "On Linux, a retained descriptor pins the original inode across unlink or replacement, keeping bundled children, workers and SSH sessions launchable."
code = "FD"
outcome = "image continuity"
[[survival]]
condition = "No child exec"
response = "If self-exec fails, child launches fail, but shell builtins, aliases, functions and doctor remain available. AXE does not fall through to PATH."
code = "SPAWN"
outcome = "controlled degradation"
[[survival]]
condition = "Network gone"
response = "Bundled applets stay available. A fresh AXE Store cache keeps working offline."
code = "NET"
outcome = "offline recovery"
[[survival]]
condition = "No root"
response = "Inspection works without root. Operations that require privileges still fail."
code = "UID"
outcome = "unprivileged insight"

[sections]
survivalTitle = "When parts of the host fail"
survivalLead = "A failed execution route does not take down the shell or disable verification. AXE keeps the capabilities that still work."
toolsTitle = "Bundled and on-demand tools"
toolsLead = "Bundled commands work immediately. AXE Store downloads specialist tools into a private, verified cache."
environmentTitle = "Inspect the host"
environmentLead = "AXE reports virtualization, containers, sandboxing and restrictions relevant to recovery."
staticTitle = "No host loader required"
staticLead = "The primary Linux release is a static musl ELF executable. It needs no loader, shell or coreutils on the target."
remoteTitle = "SSH access through an outbound connection"
remoteLead = "Run the certificate-only SSH/SFTP server on the target. A standalone relay makes it reachable over an outbound TCP+yamux or QUIC connection."
resolutionTitle = "Command lookup order"
resolutionLead = "Aliases, functions, builtins and bundled applets take precedence. AXE Store integrity failures stop execution rather than falling through to a PATH binary."
+++

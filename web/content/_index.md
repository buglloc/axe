+++
title = "AXE"
eyebrow = "Portable rescue shell"
headline = "When the system has nothing useful left."
tagline = "Assume nothing. Bring everything."
intro = "AXE is one executable with an interactive shell, Unix tools, host diagnostics, certificate-only SSH and a verified on-demand tool store. Run it on the host you need to inspect."
terminalLabel = "incident@unknown:/#"

[[survival]]
condition = "No shell"
response = "[Brush](https://github.com/reubeno/brush) provides an interactive shell and runs scripts and pipelines."
code = "SHELL"
outcome = "interactive shell"
[[survival]]
condition = "No coreutils"
response = "Bundled commands work without host coreutils or PATH."
code = "PATH"
outcome = "local toolbox"
[[survival]]
condition = "No procfs"
response = "A running Linux AXE can keep launching bundled commands when /proc becomes unavailable. Procfs-based diagnostics remain limited."
code = "PROC"
outcome = "commands remain available"
[[survival]]
condition = "Broken filesystem"
response = "AXE can use alternative execution routes, but filesystem and execution restrictions can still block commands."
code = "EXEC"
outcome = "best-effort recovery"
[[survival]]
condition = "Binary deleted"
response = "A running Linux AXE can keep launching bundled commands and SSH sessions after its executable is deleted or replaced."
code = "FD"
outcome = "continued execution"
[[survival]]
condition = "No child exec"
response = "If child execution fails, shell builtins, aliases, functions and doctor remain available. Bundled commands do not silently switch to host tools."
code = "SPAWN"
outcome = "controlled degradation"
[[survival]]
condition = "Network gone"
response = "Bundled commands stay available. Verified cached Store tools can be used in offline mode."
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
remoteLead = "Run the certificate-only SSH/SFTP server on the target. A standalone relay makes it reachable through an outbound TCP or QUIC connection."
resolutionTitle = "Command lookup order"
resolutionLead = "Aliases, functions, builtins and bundled applets take precedence. AXE Store integrity failures stop execution rather than falling through to a PATH binary."
+++

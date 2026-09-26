+++
title = "AXE"
eyebrow = "Portable rescue shell"
headline = "When the system has nothing useful left."
tagline = "Assume nothing. Bring everything."
intro = "One executable with an interactive shell, essential Unix tools, host diagnostics, certificate-only SSH, and a verified on-demand tool store. Built for the host you cannot rebuild yet."
terminalLabel = "incident@unknown:/#"

[[survival]]
condition = "No shell"
response = "[Brush](https://github.com/reubeno/brush) is built in, with interactive Reedline and a minimal backend for scripts and pipes."
code = "SHELL"
outcome = "interactive shell"
[[survival]]
condition = "No coreutils"
response = "Bundled applets ship inside AXE. Direct dispatch needs no PATH, and a failed BusyBox-style bridge is only a warning."
code = "PATH"
outcome = "local toolbox"
[[survival]]
condition = "No procfs"
response = "Missing or masked /proc removes one backend. Once Linux retains its executable descriptor, each AXE child inherits that capability without procfs."
code = "PROC"
outcome = "fd-backed self-exec"
[[survival]]
condition = "Broken filesystem"
response = "AXE tries descriptor exec first, then a probed private relay and one inode-checked path. Single-file Store tools can use sealed memfd."
code = "EXEC"
outcome = "checked fallback"
[[survival]]
condition = "Binary deleted"
response = "On Linux, a retained descriptor pins the original inode across unlink or replacement, keeping bundled children, workers and SSH sessions launchable."
code = "FD"
outcome = "image continuity"
[[survival]]
condition = "No child exec"
response = "When self-exec is unavailable, only the child launch fails. Shell keeps its builtins, aliases, functions and native doctor—without falling through to PATH."
code = "SPAWN"
outcome = "controlled degradation"
[[survival]]
condition = "Network gone"
response = "Bundled applets stay available. A fresh AXE Store cache keeps working offline."
code = "NET"
outcome = "offline recovery"
[[survival]]
condition = "No root"
response = "Inspection and recovery remain useful without capabilities; privileged operations fail without weakening the shell."
code = "UID"
outcome = "unprivileged insight"

[sections]
survivalTitle = "Built for the hostile case"
survivalLead = "AXE degrades without weakening verification. A failure removes only the capability it breaks; the rest of the environment stays usable."
toolsTitle = "A toolbox, not a bootstrap script"
toolsLead = "Bundled commands are ready immediately. Larger specialist tools are delivered by AXE Store and kept in a private verified cache."
environmentTitle = "Know where you landed"
environmentLead = "AXE identifies virtualization, containers and sandboxing, then reports the restrictions that determine what recovery actions can work."
staticTitle = "Static means it starts"
staticLead = "The primary Linux release is a musl ELF executable with no interpreter and no dynamic dependencies. It does not ask the target for a loader, shell or coreutils."
remoteTitle = "Open a door from the inside"
remoteLead = "Run the certificate-only SSH/SFTP server on the target. Reach isolated systems through one outbound relay connection over explicit TCP+yamux or QUIC transport."
resolutionTitle = "Deterministic command resolution"
resolutionLead = "Local behavior wins. AXE Store integrity failures block execution instead of silently falling through to an untrusted PATH binary."
+++

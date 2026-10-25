# brush-core patches

Source: the `brush-core` 0.5.0 package from the `reubeno/brush` git
repository, revision `b562e0586b366d0558b27019276e591856253947` (path
`brush-core`; the same fork revision is recorded in the root
`[workspace.metadata.brush-fork]`).

- Move the vendored core to the coordinated Brush git snapshot and its typed
  builtin/registry, completion, shell-state, and process APIs. The three AXE
  vendored Brush crates and the unmodified parser/interactive siblings must
  stay on this one revision; mixing the crates.io 0.5.0 API with the fork
  breaks builtin dispatch and completion contracts.
- Add the AXE process-launcher extension boundary and launch requests, so a host
  can provide native-child/job-control behavior while external commands still
  receive shell open files, working directory, environment, process group, and
  redirections.
- Add Linux descriptor-backed executable launching with a checked path fallback
  for bundled self-exec. This avoids reopening an attacker-replaced path while
  still supporting kernels/policies that reject descriptor execution.
- Keep shared open-file descriptors and descriptor duplication explicit across
  in-process subshells and native children, and resolve shell `/dev/fd` paths
  against the shell's current redirection table rather than host process fds.
- Preserve AXE's shell expansion and execution fixes from the snapshot: empty
  brace alternatives produce no word, command/path lookup and completion use
  the updated registry state, and shell errors retain structured status and
  redirection behavior.
- Add a process-global child SIGPIPE policy. Brush's upstream launcher resets
  child SIGPIPE to `SIG_DFL` unconditionally; AXE restores `SIG_IGN` for shells
  that inherited an ignored disposition while retaining default termination for
  ordinary applet processes. This is upstreamable.
- Keep a standalone manifest without upstream workspace lockfiles, benches,
  development examples, or the package's workspace-only metadata; the AXE
  workspace owns dependency resolution and the shared `Cargo.lock`.

Upstreamability: the process-launcher/descriptor and SIGPIPE policy changes are
intended as host-integration improvements for Brush. The AXE descriptor fallback
is an integration patch; no upstream issue or commit is recorded in AXE.

AXE consumer regressions are in `crates/axe/tests/smoke.rs`,
`crates/axe/tests/sigpipe_compat.rs`, `crates/axe/tests/compression_compat.rs`,
and `crates/axe/tests/linux_network.rs`.

# brush-builtins patches

Source: the `brush-builtins` 0.2.0 package from the `reubeno/brush` git
repository, revision `b562e0586b366d0558b27019276e591856253947` (path
`brush-builtins`; the same fork revision is recorded in the root
`[workspace.metadata.brush-fork]`).

- Move the builtins to the coordinated Brush git snapshot and migrate the
  builtin implementations to its `Builtin`/argument parsing, registry, shell
  state, and completion APIs. This keeps the builtins ABI and feature registry
  aligned with the vendored core and shell rather than combining incompatible
  crates.io releases.
- Keep `command` and `exec` command resolution on Brush's shell lookup and AXE's
  process-launcher boundary. External execution therefore preserves AXE's
  descriptor-backed bundled self-exec, redirections, status propagation, and
  process policy instead of bypassing the shell through a direct libc exec.
- Preserve the updated builtin feature registration and shell-state handling
  for the POSIX/bash builtin set; these are required by the fork's registry and
  are exercised through the AXE shell rather than as independent binaries.
- Keep a standalone manifest and omit upstream workspace lockfiles, CI,
  development examples, benches, and workspace-only package metadata; AXE owns
  the shared lock and registration of this library.

Upstreamability: the API migration follows the pinned Brush revision. The
process-launcher integration is an AXE host integration; no upstream issue or
commit is recorded here.

AXE consumer regressions are in `crates/axe/tests/smoke.rs`,
`crates/axe/tests/sigpipe_compat.rs`, `crates/axe/tests/compression_compat.rs`,
and `crates/axe/tests/linux_network.rs`.

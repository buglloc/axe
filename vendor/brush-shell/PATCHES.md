# brush-shell patches

Source: the `brush-shell` 0.4.0 package from the `reubeno/brush` git
repository, revision `b562e0586b366d0558b27019276e591856253947` (path
`brush-shell`; the same fork revision is recorded in the root
`[workspace.metadata.brush-fork]`).

- Move the shell to the coordinated Brush git snapshot and its matching core,
  builtins, parser, and interactive APIs. AXE pins all Brush crates to this
  revision so shell dispatch and process interfaces cannot be mixed across
  releases.
- Add AXE's bundled command registry and dispatch bridge. Bundled applets can
  run in-process through native callbacks, while commands requiring a process
  boundary use descriptor-backed self-exec and the checked path fallback from
  `brush-core`; external command lookup therefore follows the same shell
  redirections and status rules as a standalone applet.
- Keep the AXE default interactive prompt rooted in the current directory when
  no inherited prompt is supplied, while preserving an inherited `PS1` and
  non-interactive startup behavior.
- Expose the core process-global child SIGPIPE policy to shell launches. A
  shell inheriting `SIG_IGN` passes that policy to external and bundled
  children instead of allowing Brush's unconditional child reset to change
  pipeline status semantics. This is upstreamable.
- Keep a standalone manifest and remove upstream workspace-only lockfiles,
  CI/development metadata, and unrelated package scaffolding; AXE owns the
  workspace lock and command registration.

Upstreamability: the child-policy bridge is intended for Brush upstream. The
bundled registry, native callbacks, descriptor launch integration, and AXE
prompt are host integration and have no upstream issue or commit recorded here.

AXE consumer regressions are in `crates/axe/tests/smoke.rs`,
`crates/axe/tests/sigpipe_compat.rs`, `crates/axe/tests/compression_compat.rs`,
and `crates/axe/tests/linux_network.rs`.

# brush-shell patches

Source: the `brush-shell` 0.4.0 package from the `reubeno/brush` git
repository, revision `e37b3c91b7f3d2cc5a67cdc38381f480044d7a7b` (path
`brush-shell`; the same fork revision is recorded in the root
`[workspace.metadata.brush-fork]`). Everything not listed below is upstream
source at that revision.

- `bundled.rs`: registry entries carry a shell execution policy. Spawn
  entries re-exec through an executable provider consulted before every
  launch (`install_with_executable`), using the descriptor-backed self-exec
  and checked path fallback from `brush-core`; builtin entries run AXE
  callbacks in the parent shell against a `ShellSnapshot` with the
  redirected standard streams. Adds `invoke_installed` for AXE's own entry
  routing, `executable_path` for path-only callers, launch-failure
  observation for `doctor`, and `set_child_sigpipe_ignored`.
- `entry::run(default_prompt)`: sets AXE's interactive `PS1` unless one is
  inherited (an inherited empty `PS1` is kept).
- Tokio runtime per input backend: Minimal and Vt use a current-thread runtime
  so the non-interactive shell starts when thread creation is denied;
  Basic and Reedline keep a two-worker multi-thread runtime because
  Reedline uses `block_in_place`. Upstream always builds a multi-thread
  runtime on Unix.
- `human_panic` handler removed: panics print the standard Rust diagnostic to
  stderr instead of writing a report file; terminal reset is kept.
- Standalone manifest without upstream workspace lockfiles or CI metadata; AXE
  owns the workspace lock and command registration.

Upstreamability: the runtime selection and SIGPIPE bridge are candidates for
Brush upstream. The bundled registry, native callbacks, descriptor launch, and
AXE prompt are host integration. No upstream issue or PR is recorded.

AXE consumer regressions are in `crates/axe/tests/smoke.rs`
(`unlinked_binary_launches_embedded_shell`,
`inherited_runtime_root_stabilizes_child_selection`,
`doctor_reports_readable_shell_state_and_selected_environment`) and
`crates/axe/tests/sigpipe_compat.rs`.

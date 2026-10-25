# brush-builtins patches

Source: the `brush-builtins` 0.2.0 package from the `reubeno/brush` git
repository, revision `e37b3c91b7f3d2cc5a67cdc38381f480044d7a7b` (path
`brush-builtins`; the same fork revision is recorded in the root
`[workspace.metadata.brush-fork]`). Everything not listed below is upstream
source at that revision.

- `wait PID`: resolves the PID through `JobManager::resolve_process_id` from
  the patched `brush-core` and waits for that job; an unknown PID reports
  `not a child of this shell` with status 127. Upstream still returns
  "unimplemented: wait with process IDs".
- Standalone manifest without upstream workspace lockfiles, CI, examples,
  benches, or workspace-only metadata; AXE owns the shared lock.

Upstreamability: `wait PID` fixes upstream behavior. No upstream issue or PR
is recorded.

AXE consumer regressions are in `crates/axe/tests/smoke.rs`
(`shell_tracks_bundled_background_process_lifecycle`,
`shell_exposes_internal_background_jobs_as_waitable_job_specs`).

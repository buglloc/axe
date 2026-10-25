# brush-core patches

Source: the `brush-core` 0.5.0 package from the `reubeno/brush` git
repository, revision `e37b3c91b7f3d2cc5a67cdc38381f480044d7a7b` (path
`brush-core`; the same fork revision is recorded in the root
`[workspace.metadata.brush-fork]`). Everything not listed below is upstream
source at that revision.

- Linux descriptor-backed executable launching (`sys/unix/descriptor_exec.rs`,
  `SimpleCommand::executable_descriptor`, `ProcessSpawnRequest`) with a
  checked path fallback for bundled self-exec. This avoids reopening an
  attacker-replaced path while still supporting kernels/policies that reject
  descriptor execution.
- Process-global child SIGPIPE policy (`sys::sigpipe`). Upstream's launcher
  leaves std's unconditional `SIG_DFL` reset in the child; AXE restores
  `SIG_IGN` for shells that inherited an ignored disposition.
- Deterministic `$!`: an asynchronous list waits until its first pipeline has
  spawned and records that process ID on the job (`JobManager` last
  background PID/job ID, `Job::with_representative_pid`). A list without an
  operating-system process exposes `%N`. Upstream reads the current job's
  representative PID, which is always empty for the internal task that
  runs an asynchronous list.
- `JobManager::resolve_process_id` backs `wait PID` in `brush-builtins`.
- `Error::as_io_error` also exposes the I/O error of
  `FailedToExecuteCommand`, so bundled launch failures report errno.
- Identity fallback for UIDs without a passwd entry: the current user name
  falls back to `USER`/`LOGNAME` and then the numeric UID, and group IDs come
  from `getgroups(2)` plus the effective GID instead of a name-based group
  lookup, so the `\u` prompt escape and `$GROUPS` keep working.
- Standalone manifest without upstream workspace lockfiles, benches, or
  workspace-only metadata; the AXE workspace owns the shared `Cargo.lock`.

Upstreamability: `$!`, `wait PID`, the identity fallback, and the SIGPIPE
policy fix upstream behavior. Descriptor execution is AXE host integration.
No upstream issue or PR is recorded.

AXE consumer regressions are in `crates/axe/tests/smoke.rs`
(`unresolved_current_identity_uses_numeric_fallbacks`,
`shell_tracks_bundled_background_process_lifecycle`,
`shell_exposes_internal_background_jobs_as_waitable_job_specs`,
`unlinked_binary_launches_embedded_shell`) and
`crates/axe/tests/sigpipe_compat.rs`.

use std::io::Write;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::environment::{
    self, CgroupMembership, DistroObservation, EnvironmentObservation, IdMapRange,
    IsolationObservation, Observation, ObservationStatus, PressureObservation, PressureResource,
    PressureScope,
};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, Outcome, ProtocolError, RecordSink,
    check_deadline,
};

#[derive(Serialize)]
struct OverviewData<'a> {
    detail: &'static str,
    scope: &'static str,
    observed_at_unix_ms: u64,
    elapsed_ms: u64,
    environment: OverviewEnvironment<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum OverviewEnvironment<'a> {
    Summary(&'a EnvironmentSummary<'a>),
    Full(&'a EnvironmentObservation),
}

#[derive(Serialize)]
struct EnvironmentSummary<'a> {
    host: HostSummary<'a>,
    kernel: KernelSummary<'a>,
    memory: MemorySummary<'a>,
    resources: ResourceSummary<'a>,
    subsystems: SubsystemSummary<'a>,
    process: ProcessSummary<'a>,
    restrictions: RestrictionSummary<'a>,
    isolation: &'a IsolationObservation,
}

#[derive(Serialize)]
struct HostSummary<'a> {
    os: &'a Observation<String>,
    architecture: &'a Observation<String>,
    hostname: &'a Observation<String>,
    distro: &'a Observation<DistroObservation>,
}

#[derive(Serialize)]
struct KernelSummary<'a> {
    release: &'a Observation<String>,
}

#[derive(Serialize)]
struct MemorySummary<'a> {
    physical_bytes: &'a Observation<u64>,
    available_bytes: &'a Observation<u64>,
}

#[derive(Serialize)]
struct ResourceSummary<'a> {
    cpu_online: &'a Observation<u32>,
    cpu_allowed_list: &'a Observation<String>,
    uptime_seconds: &'a Observation<f64>,
    load_average: &'a Observation<[f64; 3]>,
    pressure: PressureSummary<'a>,
}

#[derive(Serialize)]
struct PressureSummary<'a> {
    scope: PressureScope,
    cpu: &'a Observation<PressureResource>,
    memory: &'a Observation<PressureResource>,
    io: &'a Observation<PressureResource>,
}

#[derive(Serialize)]
struct SubsystemSummary<'a> {
    procfs: &'a Observation<bool>,
    sysfs: &'a Observation<bool>,
    cgroup_v2: &'a Observation<bool>,
    systemd: &'a Observation<bool>,
    dbus: &'a Observation<bool>,
    porto: &'a Observation<bool>,
}

#[derive(Serialize)]
struct ProcessSummary<'a> {
    pid: &'a Observation<u32>,
    effective_uid: &'a Observation<u32>,
    effective_gid: &'a Observation<u32>,
}

#[derive(Serialize)]
struct RestrictionSummary<'a> {
    no_new_privileges: &'a Observation<bool>,
    seccomp_mode: &'a Observation<u32>,
    uid_map: &'a Observation<Vec<IdMapRange>>,
    gid_map: &'a Observation<Vec<IdMapRange>>,
    cgroups: CgroupSummary<'a>,
}

#[derive(Serialize)]
struct CgroupSummary<'a> {
    membership: &'a Observation<Vec<CgroupMembership>>,
    limits: CgroupLimitSummary<'a>,
}

#[derive(Serialize)]
struct CgroupLimitSummary<'a> {
    #[serde(rename = "cpu.max")]
    cpu_max: &'a Observation<String>,
    #[serde(rename = "memory.current")]
    memory_current: &'a Observation<String>,
    #[serde(rename = "memory.max")]
    memory_max: &'a Observation<String>,
    #[serde(rename = "pids.current")]
    pids_current: &'a Observation<String>,
    #[serde(rename = "pids.max")]
    pids_max: &'a Observation<String>,
}

impl<'a> From<&'a EnvironmentObservation> for EnvironmentSummary<'a> {
    fn from(environment: &'a EnvironmentObservation) -> Self {
        let host = &environment.host;
        let memory = &environment.memory;
        let resources = &environment.resources;
        let subsystems = &environment.subsystems;
        let process = &environment.process;
        let restrictions = &environment.restrictions;
        let cgroups = &restrictions.cgroups;
        let limits = &cgroups.limits;

        Self {
            host: HostSummary {
                os: &host.os,
                architecture: &host.architecture,
                hostname: &host.hostname,
                distro: &host.distro,
            },
            kernel: KernelSummary {
                release: &environment.kernel.release,
            },
            memory: MemorySummary {
                physical_bytes: &memory.physical_bytes,
                available_bytes: &memory.available_bytes,
            },
            resources: ResourceSummary {
                cpu_online: &resources.cpu_online,
                cpu_allowed_list: &resources.cpu_allowed_list,
                uptime_seconds: &resources.uptime_seconds,
                load_average: &resources.load_average,
                pressure: PressureSummary {
                    scope: resources.pressure.scope,
                    cpu: &resources.pressure.cpu,
                    memory: &resources.pressure.memory,
                    io: &resources.pressure.io,
                },
            },
            subsystems: SubsystemSummary {
                procfs: &subsystems.procfs,
                sysfs: &subsystems.sysfs,
                cgroup_v2: &subsystems.cgroup_v2,
                systemd: &subsystems.systemd,
                dbus: &subsystems.dbus,
                porto: &subsystems.porto,
            },
            process: ProcessSummary {
                pid: &process.pid,
                effective_uid: &process.effective_uid,
                effective_gid: &process.effective_gid,
            },
            restrictions: RestrictionSummary {
                no_new_privileges: &restrictions.no_new_privileges,
                seccomp_mode: &restrictions.seccomp_mode,
                uid_map: &restrictions.uid_map,
                gid_map: &restrictions.gid_map,
                cgroups: CgroupSummary {
                    membership: &cgroups.membership,
                    limits: CgroupLimitSummary {
                        cpu_max: &limits.cpu_max,
                        memory_current: &limits.memory_current,
                        memory_max: &limits.memory_max,
                        pids_current: &limits.pids_current,
                        pids_max: &limits.pids_max,
                    },
                },
            },
            isolation: &environment.isolation,
        }
    }
}

pub(crate) fn run<W: Write>(
    sink: &mut RecordSink<'_, W>,
    context: ExecutionContext<'_>,
    details: bool,
) -> Result<CapabilityReport, ProtocolError> {
    check_deadline(context)?;
    let started = Instant::now();
    let observed_at_unix_ms = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| ProtocolError::Internal(format!("observe wall clock: {error}")))?
            .as_millis(),
    )
    .map_err(|_| ProtocolError::Internal("observation wall clock overflows milliseconds".into()))?;
    let environment = environment::observe();
    check_deadline(context)?;
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).map_err(|_| {
        ProtocolError::Internal("observation elapsed time overflows milliseconds".into())
    })?;
    let summary;
    let (detail, environment_data) = if details {
        ("full", OverviewEnvironment::Full(&environment))
    } else {
        summary = EnvironmentSummary::from(&environment);
        ("summary", OverviewEnvironment::Summary(&summary))
    };
    let mut report = observation_report(&environment, details);
    let data = serde_json::to_value(OverviewData {
        detail,
        scope: "collector_visible",
        observed_at_unix_ms,
        elapsed_ms,
        environment: environment_data,
    })
    .map_err(ProtocolError::Encode)?;
    check_deadline(context)?;
    let limit = match sink.data("host.overview", "host_overview", "native", data) {
        Ok(()) => return Ok(report),
        Err(ProtocolError::OutputLimit) => "max_output_bytes",
        Err(ProtocolError::RecordLimit) => "max_records",
        Err(ProtocolError::LineTooLong(_)) => "max_line_bytes",
        Err(error) => return Err(error),
    };
    report.outcome = Outcome::Partial;
    report.coverage.truncated = true;
    report.coverage.skipped = report
        .coverage
        .skipped
        .saturating_add(report.coverage.observed);
    report.coverage.observed = 0;
    report.limits_hit.push(limit);
    Ok(report)
}

#[derive(Default)]
struct ObservationCoverage {
    coverage: Coverage,
    acquisition_loss: bool,
}

impl ObservationCoverage {
    fn include<T>(&mut self, observation: &Observation<T>, heuristic: bool) {
        self.coverage.scanned += 1;
        match observation.status {
            ObservationStatus::Available
            | ObservationStatus::Absent
            | ObservationStatus::NotApplicable => self.coverage.observed += 1,
            ObservationStatus::Unknown => {
                self.coverage.skipped += 1;
                self.acquisition_loss |= !heuristic;
            }
            ObservationStatus::Unsupported | ObservationStatus::Redacted => {
                self.coverage.skipped += 1;
            }
            ObservationStatus::Unavailable => {
                self.acquisition_loss = true;
                let error = observation.error.as_ref();
                let errno = error.and_then(|error| error.errno);
                let code = error.and_then(|error| error.code.as_deref());
                if matches!(errno, Some(1 | 13)) || code == Some("permissiondenied") {
                    self.coverage.denied += 1;
                } else if errno == Some(2) || code == Some("notfound") {
                    self.coverage.vanished += 1;
                } else {
                    self.coverage.skipped += 1;
                }
            }
        }
    }

    fn pressure(&mut self, pressure: &PressureObservation) {
        for observation in [&pressure.cpu, &pressure.memory, &pressure.io] {
            self.include(observation, false);
        }
    }

    fn report(self) -> CapabilityReport {
        CapabilityReport {
            outcome: if self.acquisition_loss {
                Outcome::Partial
            } else {
                Outcome::Complete
            },
            coverage: self.coverage,
            limits_hit: Vec::new(),
        }
    }
}

fn observation_report(environment: &EnvironmentObservation, details: bool) -> CapabilityReport {
    let mut coverage = ObservationCoverage::default();
    let host = &environment.host;
    for observation in [&host.os, &host.architecture, &host.hostname] {
        coverage.include(observation, false);
    }
    coverage.include(&host.distro, false);
    let kernel = &environment.kernel;
    coverage.include(&kernel.release, false);
    if details {
        for observation in [&kernel.name, &kernel.version, &kernel.command_line] {
            coverage.include(observation, false);
        }
    }
    let memory = &environment.memory;
    for observation in [&memory.physical_bytes, &memory.available_bytes] {
        coverage.include(observation, false);
    }
    if details {
        for observation in [&memory.swap_total_bytes, &memory.swap_free_bytes] {
            coverage.include(observation, false);
        }
    }
    let resources = &environment.resources;
    coverage.include(&resources.cpu_online, false);
    coverage.include(&resources.cpu_allowed_list, false);
    coverage.include(&resources.uptime_seconds, false);
    coverage.include(&resources.load_average, false);
    coverage.pressure(&resources.pressure);
    let subsystems = &environment.subsystems;
    for observation in [
        &subsystems.procfs,
        &subsystems.sysfs,
        &subsystems.cgroup_v2,
        &subsystems.systemd,
        &subsystems.dbus,
        &subsystems.porto,
    ] {
        coverage.include(observation, false);
    }
    let process = &environment.process;
    for observation in [&process.pid, &process.effective_uid, &process.effective_gid] {
        coverage.include(observation, false);
    }
    if details {
        for observation in [&process.ppid, &process.uid, &process.gid] {
            coverage.include(observation, false);
        }
    }
    let restrictions = &environment.restrictions;
    coverage.include(&restrictions.no_new_privileges, false);
    coverage.include(&restrictions.seccomp_mode, false);
    for observation in [&restrictions.uid_map, &restrictions.gid_map] {
        coverage.include(observation, false);
    }
    if details {
        let capabilities = &restrictions.capability_sets;
        for observation in [
            &capabilities.inheritable,
            &capabilities.permitted,
            &capabilities.effective,
            &capabilities.bounding,
            &capabilities.ambient,
        ] {
            coverage.include(observation, false);
        }
        coverage.include(&restrictions.seccomp_filter_count, false);
        coverage.include(&restrictions.lsm_profile, false);
        let namespaces = &restrictions.namespaces;
        for observation in [
            &namespaces.cgroup,
            &namespaces.ipc,
            &namespaces.mnt,
            &namespaces.net,
            &namespaces.pid,
            &namespaces.time,
            &namespaces.user,
            &namespaces.uts,
        ] {
            coverage.include(observation, false);
        }
    }
    let cgroups = &restrictions.cgroups;
    coverage.include(&cgroups.membership, false);
    let limits = &cgroups.limits;
    for observation in [
        &limits.cpu_max,
        &limits.memory_current,
        &limits.memory_max,
        &limits.pids_current,
        &limits.pids_max,
    ] {
        coverage.include(observation, false);
    }
    if details {
        for observation in [
            &cgroups.accounting.cpu_stat,
            &cgroups.accounting.memory_events,
            &cgroups.accounting.pids_events,
        ] {
            coverage.include(observation, false);
        }
        coverage.pressure(&cgroups.pressure);
    }
    let isolation = &environment.isolation;
    for layer in [
        &isolation.virtual_machine,
        &isolation.container,
        &isolation.sandbox,
    ] {
        coverage.include(layer, true);
    }
    coverage.report()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Failure;

    #[test]
    fn isolation_uncertainty_counts_once_per_layer_in_both_modes() {
        let mut environment = environment::observe();
        for details in [false, true] {
            environment.isolation.container =
                Observation::unknown("no strong container signature matched");
            let unknown = observation_report(&environment, details);

            environment.isolation.container =
                Observation::available(crate::environment::IsolationDetection {
                    verdict: crate::environment::IsolationVerdict::IndicatorPresent,
                    provider: "porto".into(),
                    confidence: crate::environment::Confidence::High,
                });
            let detected = observation_report(&environment, details);

            assert_eq!(detected.outcome, unknown.outcome);
            assert_eq!(detected.coverage.scanned, unknown.coverage.scanned);
            assert_eq!(detected.coverage.observed, unknown.coverage.observed + 1);
            assert_eq!(detected.coverage.skipped + 1, unknown.coverage.skipped);

            environment.isolation.container = Observation::unavailable(Failure::from_io(
                "read container evidence",
                &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ));
            let denied = observation_report(&environment, details);

            assert_eq!(denied.outcome, Outcome::Partial);
            assert_eq!(denied.coverage.denied, unknown.coverage.denied + 1);
            assert_eq!(denied.coverage.skipped + 1, unknown.coverage.skipped);
        }
    }

    #[test]
    fn normal_absence_and_inconclusive_isolation_are_complete() {
        let mut coverage = ObservationCoverage::default();
        coverage.include(&Observation::available(0_u64), false);
        coverage.include(
            &Observation::<bool>::absent("default marker is not visible"),
            false,
        );
        coverage.include(
            &Observation::<bool>::not_applicable("no v2 membership"),
            false,
        );
        coverage.include(
            &Observation::<bool>::unsupported("Linux-specific observation"),
            false,
        );
        coverage.include(
            &Observation::<bool>::unknown("no strong isolation signature matched"),
            true,
        );

        let report = coverage.report();

        assert_eq!(report.outcome, Outcome::Complete);
        assert_eq!(report.coverage.scanned, 5);
        assert_eq!(report.coverage.observed, 3);
        assert_eq!(report.coverage.skipped, 2);
        assert_eq!(report.coverage.denied, 0);
        assert!(!report.coverage.truncated);
    }

    #[test]
    fn pressure_scope_does_not_turn_missing_or_failed_resources_into_observations() {
        for scope in [
            PressureScope::VisibleSystem,
            PressureScope::VisibleCurrentCgroup,
        ] {
            let mut pressure = PressureObservation {
                scope,
                cpu: Observation::available(PressureResource {
                    some: crate::environment::PressureStall {
                        avg10: 0.0,
                        avg60: 0.0,
                        avg300: 0.0,
                        total_us: 0,
                    },
                    full: None,
                }),
                memory: Observation::absent("pressure resource is not exposed"),
                io: Observation::unknown("pressure source is malformed"),
            };
            let mut coverage = ObservationCoverage::default();
            coverage.pressure(&pressure);
            let malformed = coverage.report();
            assert_eq!(malformed.outcome, Outcome::Partial);
            assert_eq!(malformed.coverage.scanned, 3);
            assert_eq!(malformed.coverage.observed, 2);
            assert_eq!(malformed.coverage.skipped, 1);

            pressure.io = Observation::unavailable(Failure::from_io(
                "read pressure",
                &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ));
            let mut coverage = ObservationCoverage::default();
            coverage.pressure(&pressure);
            let denied = coverage.report();
            assert_eq!(denied.outcome, Outcome::Partial);
            assert_eq!(denied.coverage.scanned, 3);
            assert_eq!(denied.coverage.observed, 2);
            assert_eq!(denied.coverage.denied, 1);
        }
    }

    #[test]
    fn denied_missing_and_malformed_needed_observations_degrade() {
        let mut coverage = ObservationCoverage::default();
        coverage.include(
            &Observation::<u64>::unknown("memory source is malformed"),
            false,
        );
        coverage.include(
            &Observation::<u64>::unavailable(Failure::from_io(
                "read source",
                &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            )),
            false,
        );
        coverage.include(
            &Observation::<u64>::unavailable(Failure::from_io(
                "read source",
                &std::io::Error::from(std::io::ErrorKind::NotFound),
            )),
            false,
        );
        coverage.include(&Observation::available(0_u64), false);

        let report = coverage.report();

        assert_eq!(report.outcome, Outcome::Partial);
        assert_eq!(report.coverage.scanned, 4);
        assert_eq!(report.coverage.observed, 1);
        assert_eq!(report.coverage.skipped, 1);
        assert_eq!(report.coverage.denied, 1);
        assert_eq!(report.coverage.vanished, 1);
        assert!(!report.coverage.truncated);
        assert!(report.limits_hit.is_empty());
    }
}

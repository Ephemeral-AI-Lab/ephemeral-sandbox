use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{durable, unix_time_ms, PocError, PocResult, SCHEMA_VERSION};

const PHYSICAL_POINT_ENV: &str = "MPLA_POC_PHYSICAL_FAULT_POINT";
const PHYSICAL_ORDINAL_ENV: &str = "MPLA_POC_PHYSICAL_FAULT_ORDINAL";
const PHYSICAL_ARMED_PATH_ENV: &str = "MPLA_POC_PHYSICAL_FAULT_ARMED_PATH";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PhysicalFaultMarker {
    pub schema_version: u32,
    pub fault_point: NamedFaultPoint,
    pub ordinal: u32,
    pub process_id: u32,
    pub post_sealing: bool,
    pub operation_id: Option<String>,
    pub durable_state_paths: Vec<PathBuf>,
    pub mount_ids: Vec<u64>,
    pub armed_unix_ms: u64,
    pub marker_parent_synced: bool,
}

macro_rules! named_fault_points {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum NamedFaultPoint {
            $($variant),+
        }

        impl NamedFaultPoint {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }

            pub fn parse(name: &str) -> PocResult<Self> {
                match name {
                    $($name => Ok(Self::$variant),)+
                    _ => Err(PocError::InvalidConfig(format!(
                        "unknown compile-time faultpoint: {name}"
                    ))),
                }
            }
        }
    };
}

named_fault_points! {
    FenceBeforeClose => "fence.before-close",
    FenceAfterClose => "fence.after-close",
    FenceAfterDrain => "fence.after-drain",
    SealingBeforeWrite => "sealing.before-write",
    SealingAfterFileFsync => "sealing.after-file-fsync",
    SealingAfterDirFsync => "sealing.after-dir-fsync",
    QuiesceBeforeStop => "quiesce.before-stop",
    QuiesceAfterReap => "quiesce.after-reap",
    QuiesceAfterFdAudit => "quiesce.after-fd-audit",
    UnmountBeforeStrict => "unmount.before-strict",
    UnmountAfterStrict => "unmount.after-strict",
    FlushBeforeSyncfs => "flush.before-syncfs",
    FlushAfterSyncfs => "flush.after-syncfs",
    InventoryAfterFirst => "inventory.after-first",
    InventoryAfterStableSecond => "inventory.after-stable-second",
    OwnerBeforeIntent => "owner.before-intent",
    OwnerAfterIntentFsync => "owner.after-intent-fsync",
    OwnerBeforeCompare => "owner.before-compare",
    OwnerAfterGenerationFsync => "owner.after-generation-fsync",
    OwnerAfterJournalCommit => "owner.after-journal-commit",
    OwnerAfterSelectorRename => "owner.after-selector-rename",
    OwnerAfterSelectorDirFsync => "owner.after-selector-dir-fsync",
    OwnerBeforeReceipt => "owner.before-receipt",
    OwnerAfterReceiptDirFsync => "owner.after-receipt-dir-fsync",
    CanonicalBeforeInstall => "canonical.before-install",
    CanonicalAfterObjectFsync => "canonical.after-object-fsync",
    CanonicalAfterObjectDirFsync => "canonical.after-object-dir-fsync",
    CanonicalAfterRootManifestFsync => "canonical.after-root-manifest-fsync",
    LocatorAfterForward => "locator.after-forward",
    LocatorAfterReverse => "locator.after-reverse",
    LocatorAfterManifestFsync => "locator.after-manifest-fsync",
    LocatorAfterSelectorRename => "locator.after-selector-rename",
    LocatorAfterSelectorDirFsync => "locator.after-selector-dir-fsync",
    RefBeforeTemp => "ref.before-temp",
    RefAfterTempFsync => "ref.after-temp-fsync",
    RefAfterReplace => "ref.after-replace",
    RefAfterParentFsync => "ref.after-parent-fsync",
    ResponseLossPublish => "response-loss.publish",
    ResponseLossActivate => "response-loss.activate",
    ResponseLossRollback => "response-loss.rollback",
    ActivateAfterRefSelect => "activate.after-ref-select",
    ActivateAfterLocatorPin => "activate.after-locator-pin",
    ActivateAfterFreshOwner => "activate.after-fresh-owner",
    ActivateAfterMount => "activate.after-mount",
    ActivateAfterReady => "activate.after-ready",
    ActivateAfterBindingFsync => "activate.after-binding-fsync",
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultPoint {
    BeforeSealing,
    AfterSealingDurable,
    AfterProcessDrain,
    AfterStrictUnmount,
    AfterSyncfs,
    AfterFirstInventory,
    AfterStableAllocation,
    AfterOwnerIntent,
    AfterOwnerAdoption,
}

/// Deterministic one-shot fault injector. It never sleeps or allocates payload
/// data, so fault evidence remains scoped to the requested protocol edge.
#[derive(Clone, Debug, Default)]
pub struct FaultInjector {
    armed: BTreeSet<FaultPoint>,
    fired: BTreeSet<FaultPoint>,
}

impl FaultInjector {
    #[must_use]
    pub fn armed(points: impl IntoIterator<Item = FaultPoint>) -> Self {
        Self {
            armed: points.into_iter().collect(),
            fired: BTreeSet::new(),
        }
    }

    pub fn hit(&mut self, point: FaultPoint, post_sealing: bool) -> PocResult<()> {
        if !self.armed.contains(&point) || !self.fired.insert(point) {
            return Ok(());
        }
        let detail = format!("deterministic fault fired at {point:?}");
        if post_sealing {
            Err(PocError::RecoveryRequired(detail))
        } else {
            Err(PocError::Integrity(detail))
        }
    }

    #[must_use]
    pub fn fired(&self, point: FaultPoint) -> bool {
        self.fired.contains(&point)
    }
}

/// Host-test injector for the frozen named durable-edge registry. The physical
/// harness uses the same names to stop and kill the process at exact markers.
#[derive(Clone, Debug, Default)]
pub struct NamedFaultInjector {
    armed: BTreeSet<(NamedFaultPoint, u32)>,
    fired: BTreeSet<(NamedFaultPoint, u32)>,
    physical_operation_id: Option<String>,
    physical_state_paths: Vec<PathBuf>,
    physical_stationary_payload_path: Option<PathBuf>,
}

impl NamedFaultInjector {
    #[must_use]
    pub fn armed(points: impl IntoIterator<Item = (NamedFaultPoint, u32)>) -> Self {
        Self {
            armed: points.into_iter().collect(),
            fired: BTreeSet::new(),
            physical_operation_id: None,
            physical_state_paths: Vec::new(),
            physical_stationary_payload_path: None,
        }
    }

    #[must_use]
    pub fn with_physical_context(
        mut self,
        operation_id: impl Into<String>,
        durable_state_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        self.physical_operation_id = Some(operation_id.into());
        self.physical_state_paths = durable_state_paths.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_physical_stationary_payload_path(
        mut self,
        stationary_payload_path: impl Into<PathBuf>,
    ) -> Self {
        self.physical_stationary_payload_path = Some(stationary_payload_path.into());
        self
    }

    #[must_use]
    pub fn physical_stationary_payload_path(&self) -> Option<&Path> {
        self.physical_stationary_payload_path.as_deref()
    }

    pub fn reach(
        &mut self,
        point: NamedFaultPoint,
        ordinal: u32,
        post_sealing: bool,
    ) -> PocResult<()> {
        physical_reach(
            point,
            ordinal,
            post_sealing,
            self.physical_operation_id.as_deref(),
            &self.physical_state_paths,
        )?;
        if !self.armed.contains(&(point, ordinal)) || !self.fired.insert((point, ordinal)) {
            return Ok(());
        }
        let detail = format!("named fault fired at {} ordinal {ordinal}", point.as_str());
        if post_sealing {
            Err(PocError::RecoveryRequired(detail))
        } else {
            Err(PocError::Integrity(detail))
        }
    }

    #[must_use]
    pub fn fired(&self, point: NamedFaultPoint, ordinal: u32) -> bool {
        self.fired.contains(&(point, ordinal))
    }
}

/// Stops a disposable physical fault child at an exact named protocol edge.
///
/// The marker is durably replaced before `SIGSTOP`, allowing the host
/// supervisor to kill the stopped process without timing guesses. When the
/// physical environment is absent or names another point this is a no-op.
pub fn physical_reach(
    point: NamedFaultPoint,
    ordinal: u32,
    post_sealing: bool,
    operation_id: Option<&str>,
    durable_state_paths: &[PathBuf],
) -> PocResult<()> {
    let Some(configured) = std::env::var_os(PHYSICAL_POINT_ENV) else {
        return Ok(());
    };
    let configured = configured
        .into_string()
        .map_err(|_| PocError::InvalidConfig(format!("{PHYSICAL_POINT_ENV} is not UTF-8")))?;
    let configured = NamedFaultPoint::parse(&configured)?;
    let configured_ordinal = match std::env::var(PHYSICAL_ORDINAL_ENV) {
        Ok(value) => value.parse::<u32>().map_err(|error| {
            PocError::InvalidConfig(format!(
                "{PHYSICAL_ORDINAL_ENV} must be a positive u32: {error}"
            ))
        })?,
        Err(std::env::VarError::NotPresent) => 1,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(PocError::InvalidConfig(format!(
                "{PHYSICAL_ORDINAL_ENV} is not UTF-8"
            )));
        }
    };
    if configured_ordinal == 0 {
        return Err(PocError::InvalidConfig(format!(
            "{PHYSICAL_ORDINAL_ENV} must be positive"
        )));
    }
    if configured != point || configured_ordinal != ordinal {
        return Ok(());
    }
    let armed_path = std::env::var_os(PHYSICAL_ARMED_PATH_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| {
            PocError::InvalidConfig(format!(
                "{PHYSICAL_ARMED_PATH_ENV} is required for a physical fault"
            ))
        })?;
    let marker = PhysicalFaultMarker {
        schema_version: SCHEMA_VERSION,
        fault_point: point,
        ordinal,
        process_id: std::process::id(),
        post_sealing,
        operation_id: operation_id.map(str::to_owned),
        durable_state_paths: durable_state_paths.to_vec(),
        mount_ids: current_mount_ids(durable_state_paths)?,
        armed_unix_ms: unix_time_ms()?,
        marker_parent_synced: true,
    };
    durable::replace_json(&armed_path, &marker)?;
    stop_self()?;
    Err(if post_sealing {
        PocError::RecoveryRequired(format!(
            "physical fault process unexpectedly resumed after {} ordinal {ordinal}",
            point.as_str()
        ))
    } else {
        PocError::Integrity(format!(
            "physical fault process unexpectedly resumed after {} ordinal {ordinal}",
            point.as_str()
        ))
    })
}

fn current_mount_ids(paths: &[PathBuf]) -> PocResult<Vec<u64>> {
    #[cfg(target_os = "linux")]
    {
        let mountinfo_path = Path::new("/proc/self/mountinfo");
        let mountinfo = std::fs::read_to_string(mountinfo_path).map_err(|error| {
            PocError::io("read physical fault mountinfo", mountinfo_path, error)
        })?;
        let mut ids = BTreeSet::new();
        for line in mountinfo.lines() {
            let mut fields = line.split_ascii_whitespace();
            let Some(id) = fields.next().and_then(|field| field.parse::<u64>().ok()) else {
                continue;
            };
            let Some(mount_path) = fields.nth(3) else {
                continue;
            };
            let mount_path = Path::new(mount_path);
            if paths
                .iter()
                .any(|path| path.starts_with(mount_path) || mount_path.starts_with(path))
            {
                ids.insert(id);
            }
        }
        Ok(ids.into_iter().collect())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = paths;
        Ok(Vec::new())
    }
}

#[cfg(unix)]
fn stop_self() -> PocResult<()> {
    let process_id = i32::try_from(std::process::id())
        .map_err(|_| PocError::Integrity("process ID does not fit in pid_t".to_owned()))?;
    // SAFETY: SIGSTOP is sent only to this explicitly disposable fault child.
    let result = unsafe { libc::kill(process_id, libc::SIGSTOP) };
    if result == 0 {
        Ok(())
    } else {
        Err(PocError::io(
            "SIGSTOP physical fault child",
            Path::new("/proc/self"),
            std::io::Error::last_os_error(),
        ))
    }
}

#[cfg(not(unix))]
fn stop_self() -> PocResult<()> {
    Err(PocError::InvalidConfig(
        "physical fault SIGSTOP is unsupported on this platform".to_owned(),
    ))
}

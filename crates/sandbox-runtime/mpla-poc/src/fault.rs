use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{PocError, PocResult};

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
}

impl NamedFaultInjector {
    #[must_use]
    pub fn armed(points: impl IntoIterator<Item = (NamedFaultPoint, u32)>) -> Self {
        Self {
            armed: points.into_iter().collect(),
            fired: BTreeSet::new(),
        }
    }

    pub fn reach(
        &mut self,
        point: NamedFaultPoint,
        ordinal: u32,
        post_sealing: bool,
    ) -> PocResult<()> {
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

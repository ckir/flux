//! Pure mount-root judgments shared by the directory-handle arm and `StdFileSystem::mount_root`.

use flux_fs::MountRoot;

/// The kernel's own answer, when it gave one: `None` when `mask` does not say `MOUNT_ROOT` was reported.
#[cfg(target_os = "linux")]
pub(crate) fn from_statx_attributes(
    mask: rustix::fs::StatxAttributes,
    attrs: rustix::fs::StatxAttributes,
) -> Option<MountRoot> {
    use rustix::fs::StatxAttributes;
    if !mask.contains(StatxAttributes::MOUNT_ROOT) {
        return None;
    }
    Some(if attrs.contains(StatxAttributes::MOUNT_ROOT) { MountRoot::Yes } else { MountRoot::No })
}

/// Judge by device numbers: differing devices are a mount root; equal ones are `No` on macOS (no same-filesystem
/// bind mount of its own) and `Unknown` elsewhere.
pub(crate) fn from_devices(child_dev: u64, parent_dev: u64) -> MountRoot {
    if child_dev != parent_dev {
        MountRoot::Yes
    } else if cfg!(target_os = "macos") {
        MountRoot::No
    } else {
        MountRoot::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_devices_are_a_mount_root() {
        assert_eq!(from_devices(1, 2), MountRoot::Yes);
    }

    #[test]
    fn equal_devices_cannot_tell_on_linux_and_are_no_on_macos() {
        if cfg!(target_os = "linux") {
            assert_eq!(from_devices(3, 3), MountRoot::Unknown);
        }
        if cfg!(target_os = "macos") {
            assert_eq!(from_devices(3, 3), MountRoot::No);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn statx_decides_only_when_the_kernel_reports_the_attribute() {
        use rustix::fs::StatxAttributes;
        assert_eq!(
            from_statx_attributes(StatxAttributes::empty(), StatxAttributes::MOUNT_ROOT),
            None
        );
        assert_eq!(
            from_statx_attributes(StatxAttributes::MOUNT_ROOT, StatxAttributes::MOUNT_ROOT),
            Some(MountRoot::Yes)
        );
        assert_eq!(
            from_statx_attributes(StatxAttributes::MOUNT_ROOT, StatxAttributes::empty()),
            Some(MountRoot::No)
        );
    }
}

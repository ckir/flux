// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Cut 9a's compatibility check for `--resume` (spec 121, "Compatibility"): whether a prior operation's stored
//! configuration admits this run. Pure: it reads the prior's state and the run's roots and options, and decides.

use super::{RunError, RunWarning};
use crate::lock::{LockCode, LockError};
use crate::prior::{PriorOp, corrupt};
use crate::state::{
    Config, FORMAT_VERSION, Options, Root, V1, V2, from_native_hex, parse_identity,
};
use flux_fs::{CopyOptions, FileIdentity};

fn refused(code: LockCode, detail: String) -> RunError {
    RunError::Refused {
        refusal: Box::new(crate::lock::Refusal { code, holder: None, detail }),
        changed: false,
        not_removed: None,
    }
}

fn incompatible(detail: String) -> RunError {
    refused(LockCode::IncompatibleState, detail)
}

/// A stored `native_hex` path as the message shows it: decoded, or the hex itself when it does not decode.
fn shown(hex: &str) -> String {
    from_native_hex(hex).map_or_else(|| hex.to_string(), |p| p.display().to_string())
}

/// The spec's Compatibility table, in this order: format, fingerprint, roots, the four exact-match options,
/// durability. `Ok` is the `Config` the adopted manifest will carry (the stored one, `durability` set to "strict" on
/// a normal -> strict resume). Every `Err` is `RunError::Refused { changed: false, not_removed: None }`.
pub(crate) fn validate(
    prior: &PriorOp,
    roots: &[Root],
    opts: &CopyOptions,
    warnings: &mut Vec<RunWarning>,
) -> Result<Config, RunError> {
    let state = &prior.state;
    let stored = match (&state.config, state.format_version) {
        (Some(config), _) => config,
        (None, version) => {
            debug_assert!(version == V1 || version == V2);
            return Err(incompatible(format!(
                "operation {} was created by an older version (format {version}) and has no options to compare; run again with --restart",
                state.operation_id
            )));
        }
    };
    // A format-3 prior HAS a config, so the arm above did not catch it; it carries no WAL generation to continue.
    if state.format_version < FORMAT_VERSION {
        return Err(incompatible(format!(
            "operation {} was created by an older version (format {}) and has no WAL generation to continue; run again with --restart",
            state.operation_id, state.format_version
        )));
    }
    if stored.options.fingerprint() != stored.configuration_fingerprint {
        return Err(
            match corrupt(
                &prior.shown,
                "the configuration fingerprint does not match the stored options",
            ) {
                LockError::Refused(refusal) => {
                    RunError::Refused { refusal, changed: false, not_removed: None }
                }
                LockError::Io(error) => unreachable!("corrupt builds a refusal, not {error:?}"),
            },
        );
    }
    let mut by_path = None;
    if roots.len() != stored.roots.len() {
        return Err(incompatible(format!(
            "the source roots differ: the operation has {} roots, this run names {}; run again with --restart to supersede it",
            stored.roots.len(),
            roots.len()
        )));
    }
    if let (Some(then), Some(now)) = (stored.roots.first(), roots.first()) {
        let strong = |text: &str| match parse_identity(text) {
            Some(id @ FileIdentity::Strong(_)) => Some(id),
            _ => None,
        };
        match (strong(&then.source_identity), strong(&now.source_identity)) {
            (Some(a), Some(b)) if a == b => {}
            (Some(_), Some(_)) => return Err(source_mismatch(Some(then), Some(now))),
            _ if then.source_root == now.source_root => {
                by_path = Some(from_native_hex(&now.source_root).unwrap_or_default());
            }
            _ => return Err(source_mismatch(Some(then), Some(now))),
        }
        if then.destination_prefix != now.destination_prefix {
            return Err(incompatible(format!(
                "the destination differs: the operation writes {} and this run names {}; run again with --restart to supersede it",
                shown(&then.destination_prefix),
                shown(&now.destination_prefix)
            )));
        }
    }
    let now = Options::of(opts);
    let then = &stored.options;
    let (then_cf, now_cf) = (then.cross_filesystems.to_string(), now.cross_filesystems.to_string());
    for (name, was, asks) in [
        ("preserve_times", &then.preserve_times, &now.preserve_times),
        ("preserve_permissions", &then.preserve_permissions, &now.preserve_permissions),
        ("safety", &then.safety, &now.safety),
        ("existing", &then.existing, &now.existing),
        ("links", &then.links, &now.links),
        ("cross_filesystems", &then_cf, &now_cf),
    ] {
        if was != asks {
            return Err(incompatible(format!(
                "{name}: the operation runs with {was}, this run asks {asks}; run again with the same option, or with --restart to supersede it"
            )));
        }
    }
    let mut config = stored.clone();
    if then.durability != now.durability {
        if then.durability == "strict" {
            return Err(incompatible(
                "durability: the operation runs with strict durability; pass --durability strict"
                    .to_string(),
            ));
        }
        config.options.durability = now.durability;
    }
    if let Some(path) = by_path {
        warnings.push(RunWarning::ResumeMappingByPath(path));
    }
    Ok(config)
}

fn source_mismatch(then: Option<&Root>, now: Option<&Root>) -> RunError {
    let describe = |r: Option<&Root>| {
        r.map_or_else(
            || ("no source root".to_string(), "none".to_string()),
            |r| (shown(&r.source_root), r.source_identity.clone()),
        )
    };
    let (then_path, then_id) = describe(then);
    let (now_path, now_id) = describe(now);
    incompatible(format!(
        "the source root differs: the operation copies {then_path} ({then_id}), this run names {now_path} ({now_id}); run again with --restart to supersede it"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Config, Kind, OperationState, Options, Root, native_hex};
    use flux_fs::{
        CopyOptions, Durability, ExistingPolicy, Links, OperationId, Preserve, Publish, Safety,
    };
    use std::path::PathBuf;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn opts() -> CopyOptions {
        CopyOptions {
            preserve_times: Preserve::Default,
            preserve_permissions: Preserve::Default,
            durability: Durability::Normal,
            publish: Publish::Replace,
            safety: Safety::Default,
            operation_id: OperationId::new("replaced by the run"),
            existing: ExistingPolicy::Overwrite,
            links: Links::Copy,
            hardlinks: flux_fs::Hardlinks::Auto,
            cross_filesystems: false,
            verify: flux_fs::VerifyLevel::None,
            hash: flux_fs::HashAlgorithm::Blake3,
        }
    }

    fn root(path: &str, identity: &str) -> Root {
        Root {
            source_root: native_hex(std::path::Path::new(path)),
            source_identity: identity.to_string(),
            destination_prefix: String::new(),
            kind: None,
        }
    }

    fn roots_now() -> Vec<Root> {
        vec![root("/s", "strong:3:9")]
    }

    fn shown() -> PathBuf {
        PathBuf::from(format!("/d/.flux/operations/{ID}/manifest"))
    }

    /// A version-3 prior whose stored config is edited (the fingerprint is NOT recomputed).
    fn prior3(edit: impl FnOnce(&mut Config)) -> PriorOp {
        let mut config = Config::new(roots_now(), Options::of(&opts()));
        edit(&mut config);
        let state =
            OperationState::created(ID, Kind::Tree, std::path::Path::new("/d"), 1, None, config);
        PriorOp { state, shown: shown() }
    }

    /// A prior whose stored options are edited and whose fingerprint matches them.
    fn prior_options(edit: impl FnOnce(&mut Options)) -> PriorOp {
        prior3(|c| {
            edit(&mut c.options);
            c.configuration_fingerprint = c.options.fingerprint();
        })
    }

    fn refused(r: Result<Config, RunError>) -> (LockCode, String) {
        match r {
            Err(RunError::Refused { refusal, changed: false, not_removed: None }) => {
                (refusal.code, refusal.detail)
            }
            other => panic!("expected an unchanged refusal, got {other:?}"),
        }
    }

    fn run(
        prior: &PriorOp,
        roots: &[Root],
        o: &CopyOptions,
    ) -> (Result<Config, RunError>, Vec<RunWarning>) {
        let mut w = Vec::new();
        let r = validate(prior, roots, o, &mut w);
        (r, w)
    }

    #[test]
    fn a_matching_prior_validates_to_its_stored_config_with_no_warning() {
        let prior = prior3(|_| {});
        let (r, w) = run(&prior, &roots_now(), &opts());
        assert_eq!(r.unwrap(), prior.state.config.clone().unwrap());
        assert!(w.is_empty());
    }

    #[test]
    fn a_format_1_or_2_prior_is_incompatible_naming_its_format() {
        let v1 = OperationState::created_v1(ID, Kind::Tree, std::path::Path::new("/d"), 1);
        let v2 = OperationState::created_v2(ID, Kind::Tree, std::path::Path::new("/d"), 1, None);
        for (state, n) in [(v2, 2), (v1, 1)] {
            let prior = PriorOp { state, shown: shown() };
            let (code, detail) = refused(run(&prior, &roots_now(), &opts()).0);
            assert_eq!(code, LockCode::IncompatibleState);
            assert!(detail.contains(&format!("format {n}")), "{detail}");
            assert!(detail.contains("--restart"), "{detail}");
            assert!(detail.contains(ID), "{detail}");
        }
    }

    #[test]
    fn a_tampered_fingerprint_is_state_corrupt() {
        let prior = prior3(|c| c.configuration_fingerprint = "0".repeat(64));
        let (code, detail) = refused(run(&prior, &roots_now(), &opts()).0);
        assert_eq!(code, LockCode::StateCorrupt);
        assert!(detail.contains("fingerprint") && detail.contains("remove it by hand"), "{detail}");
    }

    #[test]
    fn strong_identities_decide_and_the_path_is_ignored() {
        let moved = prior3(|c| c.roots[0] = root("/elsewhere", "strong:3:9"));
        let (r, w) = run(&moved, &roots_now(), &opts());
        assert!(r.is_ok());
        assert!(w.is_empty());

        let other = prior3(|c| c.roots[0] = root("/s", "strong:3:10"));
        let (code, detail) = refused(run(&other, &roots_now(), &opts()).0);
        assert_eq!(code, LockCode::IncompatibleState);
        assert!(detail.contains("source root"), "{detail}");
        assert!(detail.contains("--restart"), "{detail}");
    }

    #[test]
    fn a_weak_side_falls_back_to_the_path_and_warns() {
        let same = prior3(|c| c.roots[0] = root("/s", "weak:3:9"));
        let (r, w) = run(&same, &roots_now(), &opts());
        assert!(r.is_ok());
        assert_eq!(w.len(), 1);
        assert!(matches!(&w[0], RunWarning::ResumeMappingByPath(p) if p == &PathBuf::from("/s")));

        let moved = prior3(|c| c.roots[0] = root("/elsewhere", "weak:3:9"));
        let (code, detail) = refused(run(&moved, &roots_now(), &opts()).0);
        assert_eq!(code, LockCode::IncompatibleState);
        assert!(detail.contains("source root"), "{detail}");
    }

    #[test]
    fn a_different_destination_prefix_is_refused() {
        let prior =
            prior3(|c| c.roots[0].destination_prefix = native_hex(std::path::Path::new("a")));
        let (code, detail) = refused(run(&prior, &roots_now(), &opts()).0);
        assert_eq!(code, LockCode::IncompatibleState);
        assert!(detail.starts_with("the destination differs"), "{detail}");
    }

    #[test]
    fn a_different_root_count_is_a_source_root_mismatch() {
        let prior = prior3(|_| {});
        let two = vec![root("/s", "strong:3:9"), root("/t", "strong:3:8")];
        let (code, detail) = refused(run(&prior, &two, &opts()).0);
        assert_eq!(code, LockCode::IncompatibleState);
        assert!(
            detail.starts_with(
                "the source roots differ: the operation has 1 roots, this run names 2;"
            ),
            "{detail}"
        );
        assert!(detail.contains("--restart"), "{detail}");
    }

    #[test]
    fn a_one_root_run_refuses_a_two_root_prior_as_a_source_mismatch() {
        // A several-source prior (format 5, two roots) met by a one-source run: the existing source-root refusal.
        let two = vec![
            Root {
                kind: Some(crate::state::RootKind::Tree),
                destination_prefix: native_hex(std::path::Path::new("s")),
                ..root("/s", "strong:3:9")
            },
            Root {
                kind: Some(crate::state::RootKind::File),
                destination_prefix: native_hex(std::path::Path::new("t")),
                ..root("/t", "strong:3:8")
            },
        ];
        let state = OperationState::created(
            ID,
            Kind::Tree,
            std::path::Path::new("/d"),
            1,
            None,
            Config::new(two, Options::of(&opts())),
        );
        assert_eq!(state.format_version, crate::state::MULTI_ROOT_FORMAT);
        let prior = PriorOp { state, shown: shown() };
        let (code, detail) = refused(run(&prior, &roots_now(), &opts()).0);
        assert_eq!(code, LockCode::IncompatibleState);
        assert!(
            detail.starts_with(
                "the source roots differ: the operation has 2 roots, this run names 1;"
            ),
            "{detail}"
        );
        assert!(detail.contains("--restart"), "{detail}");
    }

    #[test]
    fn each_exact_match_option_is_compared_and_named() {
        type Edit = fn(&mut Options);
        let cases: [(&str, Edit); 4] = [
            ("preserve_times", |o| o.preserve_times = "strict".into()),
            ("preserve_permissions", |o| o.preserve_permissions = "strict".into()),
            ("safety", |o| o.safety = "strict".into()),
            ("existing", |o| o.existing = "update".into()),
        ];
        for (name, edit) in cases {
            let prior = prior_options(edit);
            let (code, detail) = refused(run(&prior, &roots_now(), &opts()).0);
            assert_eq!(code, LockCode::IncompatibleState, "{name}");
            assert!(detail.starts_with(&format!("{name}:")), "{detail}");
            assert!(detail.contains("--restart"), "{detail}");
        }
    }

    #[test]
    fn a_different_cross_filesystems_setting_is_incompatible_and_named() {
        let crossing = CopyOptions { cross_filesystems: true, ..opts() };
        let cases = [
            (prior_options(|o| o.cross_filesystems = true), opts()),
            (prior_options(|_| {}), crossing),
        ];
        for (prior, now) in cases {
            let (code, detail) = refused(run(&prior, &roots_now(), &now).0);
            assert_eq!(code, LockCode::IncompatibleState);
            assert!(detail.starts_with("cross_filesystems:"), "{detail}");
            assert!(detail.contains("--restart"), "{detail}");
        }
    }

    #[test]
    fn the_same_cross_filesystems_setting_is_compatible_either_way() {
        let crossing = CopyOptions { cross_filesystems: true, ..opts() };
        let prior = prior_options(|o| o.cross_filesystems = true);
        let (r, w) = run(&prior, &roots_now(), &crossing);
        assert!(r.is_ok(), "{:?}", r.err());
        assert!(w.is_empty());
        let prior = prior_options(|_| {});
        let (r, w) = run(&prior, &roots_now(), &opts());
        assert!(r.is_ok(), "{:?}", r.err());
        assert!(w.is_empty());
    }

    #[test]
    fn durability_normal_to_strict_is_recorded_and_strict_to_normal_is_refused() {
        let normal = prior_options(|_| {});
        let strict_run = CopyOptions { durability: Durability::Strict, ..opts() };
        let (r, w) = run(&normal, &roots_now(), &strict_run);
        assert_eq!(r.unwrap().options.durability, "strict");
        assert!(w.is_empty());

        let strict = prior_options(|o| o.durability = "strict".into());
        let (code, detail) = refused(run(&strict, &roots_now(), &opts()).0);
        assert_eq!(code, LockCode::IncompatibleState);
        assert_eq!(
            detail,
            "durability: the operation runs with strict durability; pass --durability strict"
        );
    }
}

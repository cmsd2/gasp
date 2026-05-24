//! Planning + execution of `gasp sync`. The plan phase decides — given a
//! [`crate::status::RepoStatus`] and a [`ConflictMode`] — what action a
//! repo needs. The execute phase carries that action out.

use std::path::Path;

use crate::error::Result;
use crate::git;
use crate::manifest::Repo;
use crate::status::{HeadCompare, RepoState, RepoStatus, TargetState};
use crate::workspace::Workspace;

/// What to do when an existing repo can't be brought to the target by a
/// simple fast-forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictMode {
    /// Skip the repo and report. Default.
    Refuse,
    /// Rebase local commits onto the target.
    Rebase,
    /// Hard-reset to the target. Destructive.
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Repo doesn't exist on disk; clone it.
    Clone,
    /// HEAD already at target sha. No-op.
    AlreadyOnTarget,
    /// Manifest didn't specify a revision; nothing to update to.
    NoTarget,
    /// HEAD is behind target on the same line of history. Fast-forward.
    FastForward { from: String, to: String },
    /// Diverged or ahead; rebase local commits onto target.
    Rebase { onto: String },
    /// Hard-reset to target. Destroys local commits / changes.
    Reset { to: String },
    /// Skip with reason.
    Skip { reason: String },
}

/// Decide what to do with a repo given its current status and the chosen
/// conflict mode.
pub fn plan_action(status: &RepoStatus, mode: ConflictMode) -> Action {
    match &status.state {
        RepoState::Missing => Action::Clone,
        RepoState::NotARepo => Action::Skip {
            reason: "path exists but is not a git repository".into(),
        },
        RepoState::Present(info) => plan_present(info, mode),
    }
}

fn plan_present(info: &crate::status::RepoInfo, mode: ConflictMode) -> Action {
    // Dirty trees are precious. Only --reset is allowed to clobber.
    if info.dirty {
        return match (mode, &info.target) {
            (ConflictMode::Reset, TargetState::Resolved { sha, .. }) => {
                Action::Reset { to: sha.clone() }
            }
            (ConflictMode::Reset, _) => Action::Skip {
                reason: "--reset has no resolvable target to reset to".into(),
            },
            _ => Action::Skip {
                reason: "uncommitted changes (use --reset to discard, or commit/stash first)"
                    .into(),
            },
        };
    }

    match &info.target {
        TargetState::Unspecified => Action::NoTarget,
        TargetState::Unresolved { revision } => Action::Skip {
            reason: format!("target '{revision}' could not be resolved locally"),
        },
        TargetState::Resolved {
            sha, comparison, ..
        } => plan_resolved(&info.head, sha, comparison, mode),
    }
}

fn plan_resolved(head: &str, target: &str, comparison: &HeadCompare, mode: ConflictMode) -> Action {
    match comparison {
        HeadCompare::OnTarget => Action::AlreadyOnTarget,
        HeadCompare::Behind { .. } => Action::FastForward {
            from: head.to_string(),
            to: target.to_string(),
        },
        HeadCompare::Ahead { commits } => match mode {
            ConflictMode::Refuse => Action::Skip {
                reason: format!("local is {commits} commit(s) ahead of target"),
            },
            ConflictMode::Rebase => Action::Skip {
                reason: "rebase would be a no-op when local is purely ahead of target".into(),
            },
            ConflictMode::Reset => Action::Reset {
                to: target.to_string(),
            },
        },
        HeadCompare::Diverged { ahead, behind } => match mode {
            ConflictMode::Refuse => Action::Skip {
                reason: format!("diverged from target (ahead {ahead}, behind {behind})"),
            },
            ConflictMode::Rebase => Action::Rebase {
                onto: target.to_string(),
            },
            ConflictMode::Reset => Action::Reset {
                to: target.to_string(),
            },
        },
        HeadCompare::Unknown => match mode {
            ConflictMode::Refuse => Action::Skip {
                reason: "cannot determine relation to target (graph unknown)".into(),
            },
            ConflictMode::Rebase => Action::Rebase {
                onto: target.to_string(),
            },
            ConflictMode::Reset => Action::Reset {
                to: target.to_string(),
            },
        },
    }
}

/// Carry out a planned action against an on-disk repo path. Caller is
/// responsible for selecting the path via [`Workspace::repo_path`].
pub fn execute(workspace: &Workspace, repo: &Repo, action: &Action) -> Result<()> {
    let dest = workspace.repo_path(&repo.path);
    match action {
        Action::Clone => git::clone(&repo.url, &dest, repo.revision.as_deref()),
        Action::AlreadyOnTarget | Action::NoTarget | Action::Skip { .. } => Ok(()),
        Action::FastForward { to, .. } => git::remote::merge_ff_only(&dest, to),
        Action::Rebase { onto } => git::remote::rebase(&dest, onto),
        Action::Reset { to } => git::remote::reset_hard(&dest, to),
    }
}

/// Fetch all configured remotes for a repo (currently just the named
/// remote on the [`Repo`]).
pub fn fetch_remote(repo_path: &Path, remote: &str) -> Result<()> {
    git::remote::fetch(repo_path, remote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{HeadCompare, RepoInfo, RepoState, RepoStatus, TargetState};
    use std::path::PathBuf;

    fn present(info: RepoInfo) -> RepoStatus {
        RepoStatus {
            name: "r".into(),
            path: PathBuf::from("r"),
            state: RepoState::Present(info),
            worktrees: Vec::new(),
        }
    }

    fn info(dirty: bool, head: &str, target_sha: Option<&str>, cmp: HeadCompare) -> RepoInfo {
        let target = match target_sha {
            None => TargetState::Unspecified,
            Some(s) => TargetState::Resolved {
                revision: "main".into(),
                sha: s.to_string(),
                comparison: cmp,
            },
        };
        RepoInfo {
            head: head.into(),
            branch: Some("main".into()),
            dirty,
            target,
            has_upstream: true,
        }
    }

    fn missing() -> RepoStatus {
        RepoStatus {
            name: "r".into(),
            path: PathBuf::from("r"),
            state: RepoState::Missing,
            worktrees: Vec::new(),
        }
    }

    #[test]
    fn missing_plans_clone() {
        assert_eq!(plan_action(&missing(), ConflictMode::Refuse), Action::Clone);
        assert_eq!(plan_action(&missing(), ConflictMode::Rebase), Action::Clone);
        assert_eq!(plan_action(&missing(), ConflictMode::Reset), Action::Clone);
    }

    #[test]
    fn on_target_is_noop() {
        let s = present(info(false, "abc", Some("abc"), HeadCompare::OnTarget));
        assert_eq!(
            plan_action(&s, ConflictMode::Refuse),
            Action::AlreadyOnTarget
        );
    }

    #[test]
    fn behind_always_fast_forwards() {
        let s = present(info(
            false,
            "old",
            Some("new"),
            HeadCompare::Behind { commits: 2 },
        ));
        for mode in [
            ConflictMode::Refuse,
            ConflictMode::Rebase,
            ConflictMode::Reset,
        ] {
            assert_eq!(
                plan_action(&s, mode),
                Action::FastForward {
                    from: "old".into(),
                    to: "new".into(),
                }
            );
        }
    }

    #[test]
    fn ahead_skips_under_refuse() {
        let s = present(info(
            false,
            "new",
            Some("old"),
            HeadCompare::Ahead { commits: 1 },
        ));
        assert!(matches!(
            plan_action(&s, ConflictMode::Refuse),
            Action::Skip { .. }
        ));
    }

    #[test]
    fn ahead_resets_under_reset() {
        let s = present(info(
            false,
            "new",
            Some("old"),
            HeadCompare::Ahead { commits: 1 },
        ));
        assert_eq!(
            plan_action(&s, ConflictMode::Reset),
            Action::Reset { to: "old".into() }
        );
    }

    #[test]
    fn diverged_skips_under_refuse() {
        let s = present(info(
            false,
            "h",
            Some("t"),
            HeadCompare::Diverged {
                ahead: 2,
                behind: 3,
            },
        ));
        assert!(matches!(
            plan_action(&s, ConflictMode::Refuse),
            Action::Skip { .. }
        ));
    }

    #[test]
    fn diverged_rebases_under_rebase() {
        let s = present(info(
            false,
            "h",
            Some("t"),
            HeadCompare::Diverged {
                ahead: 2,
                behind: 3,
            },
        ));
        assert_eq!(
            plan_action(&s, ConflictMode::Rebase),
            Action::Rebase { onto: "t".into() }
        );
    }

    #[test]
    fn diverged_resets_under_reset() {
        let s = present(info(
            false,
            "h",
            Some("t"),
            HeadCompare::Diverged {
                ahead: 2,
                behind: 3,
            },
        ));
        assert_eq!(
            plan_action(&s, ConflictMode::Reset),
            Action::Reset { to: "t".into() }
        );
    }

    #[test]
    fn dirty_skips_under_refuse_and_rebase() {
        let s = present(info(true, "h", Some("t"), HeadCompare::OnTarget));
        assert!(matches!(
            plan_action(&s, ConflictMode::Refuse),
            Action::Skip { .. }
        ));
        assert!(matches!(
            plan_action(&s, ConflictMode::Rebase),
            Action::Skip { .. }
        ));
    }

    #[test]
    fn dirty_resets_to_target_under_reset() {
        let s = present(info(
            true,
            "abc",
            Some("xyz"),
            HeadCompare::Behind { commits: 1 },
        ));
        // --reset on a dirty tree clobbers and moves to target.
        assert_eq!(
            plan_action(&s, ConflictMode::Reset),
            Action::Reset { to: "xyz".into() }
        );
    }

    #[test]
    fn dirty_with_no_target_under_reset_skips() {
        let s = present(info(true, "abc", None, HeadCompare::OnTarget));
        assert!(matches!(
            plan_action(&s, ConflictMode::Reset),
            Action::Skip { .. }
        ));
    }

    #[test]
    fn no_target_is_no_op() {
        let s = present(info(false, "h", None, HeadCompare::OnTarget));
        assert_eq!(plan_action(&s, ConflictMode::Refuse), Action::NoTarget);
    }

    #[test]
    fn unresolved_target_skips() {
        let mut info = info(false, "h", None, HeadCompare::OnTarget);
        info.target = TargetState::Unresolved {
            revision: "v9".into(),
        };
        let s = present(info);
        assert!(matches!(
            plan_action(&s, ConflictMode::Refuse),
            Action::Skip { .. }
        ));
    }
}

//! Remote (cloud) agent task — portable subset.
//!
//! Maps to: CC `tasks/RemoteAgentTask/RemoteAgentTask.tsx:1-1102`.
//!
//! SEAM (runtime missing): the task runtime — `spawnRemoteAgentTask`, the
//! `pollRemoteSessionEvents` loop, `restoreRemoteAgentTasks`, the ultraplan /
//! remote-review log scanners (`extractPlanFromLog` / `extractReviewFromLog`),
//! `RemoteAgentTask.kill` (archiveRemoteSession), the remote-agent metadata
//! sidecar, and the notifications — sits on `utils/teleport.ts` +
//! `utils/teleport/api.ts` (session polling/fetch/archive), the
//! `SDKMessage` log types, the sessionStorage remote-agent sidecar, and
//! `emitTaskTerminatedSdk`, none of which are ported. `RemoteAgentTaskState`
//! itself (CC `:58-95`, with `todoList`/`log`/`reviewProgress`/ultraplan
//! fields) therefore has no `TaskState` union variant yet; the UI consumes the
//! prop-level `RemoteSessionDetailData` / `RemoteSessionProgressData`
//! projections instead. This file lands the dependency-free vocabulary so the
//! poll-stack batch has its owner in place.

use crate::utils::background::remote::remote_session::BackgroundRemoteSessionPrecondition;

/// Maps to: CC `RemoteAgentTask.tsx:97-104` `REMOTE_TASK_TYPES` /
/// `RemoteTaskType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RemoteTaskType {
    RemoteAgent,
    Ultraplan,
    Ultrareview,
    AutofixPr,
    BackgroundPr,
}

impl RemoteTaskType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RemoteAgent => "remote-agent",
            Self::Ultraplan => "ultraplan",
            Self::Ultrareview => "ultrareview",
            Self::AutofixPr => "autofix-pr",
            Self::BackgroundPr => "background-pr",
        }
    }
}

/// Maps to: CC `RemoteAgentTask.tsx:106-108` `isRemoteTaskType`.
pub fn is_remote_task_type(value: Option<&str>) -> Option<RemoteTaskType> {
    match value.unwrap_or("") {
        "remote-agent" => Some(RemoteTaskType::RemoteAgent),
        "ultraplan" => Some(RemoteTaskType::Ultraplan),
        "ultrareview" => Some(RemoteTaskType::Ultrareview),
        "autofix-pr" => Some(RemoteTaskType::AutofixPr),
        "background-pr" => Some(RemoteTaskType::BackgroundPr),
        _ => None,
    }
}

/// Maps to: CC `RemoteAgentTask.tsx:110-116` `AutofixPrRemoteTaskMetadata` /
/// `RemoteTaskMetadata` (task-specific metadata: PR number, repo, etc.).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutofixPrRemoteTaskMetadata {
    pub owner: String,
    pub repo: String,
    pub pr_number: u64,
}

pub type RemoteTaskMetadata = AutofixPrRemoteTaskMetadata;

/// Maps to: CC `RemoteAgentTask.tsx:171-178` `RemoteAgentPreconditionResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteAgentPreconditionResult {
    Eligible,
    Ineligible {
        errors: Vec<BackgroundRemoteSessionPrecondition>,
    },
}

/// Maps to: CC `RemoteAgentTask.tsx:198-215` `formatPreconditionError`.
pub fn format_precondition_error(error: BackgroundRemoteSessionPrecondition) -> &'static str {
    match error {
        BackgroundRemoteSessionPrecondition::NotLoggedIn => {
            "Please run /login and sign in with your Claude.ai account (not Console)."
        }
        BackgroundRemoteSessionPrecondition::NoRemoteEnvironment => {
            "No cloud environment available. Set one up at https://claude.ai/code/onboarding?magic=env-setup"
        }
        BackgroundRemoteSessionPrecondition::NotInGitRepo => {
            "Background tasks require a git repository. Initialize git or run from a git repository."
        }
        BackgroundRemoteSessionPrecondition::NoGitRemote => {
            "Background tasks require a GitHub remote. Add one with `git remote add origin REPO_URL`."
        }
        BackgroundRemoteSessionPrecondition::GithubAppNotInstalled => {
            "The Claude GitHub app must be installed on this repository first.\nhttps://github.com/apps/claude/installations/new"
        }
        BackgroundRemoteSessionPrecondition::PolicyBlocked => {
            "Remote sessions are disabled by your organization's policy. Contact your organization admin to enable them."
        }
    }
}

/// Maps to: CC `RemoteAgentTask.tsx:1100-1102` `getRemoteTaskSessionUrl` —
/// `getRemoteSessionUrl(sessionId, process.env.SESSION_INGRESS_URL)`.
pub fn get_remote_task_session_url(session_id: &str) -> String {
    let ingress = crate::utils::process_env::var("SESSION_INGRESS_URL");
    crate::bridge::bridge_status_util::get_remote_session_url(session_id, ingress.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_task_type_roundtrip_matches_official_literals() {
        for task_type in [
            RemoteTaskType::RemoteAgent,
            RemoteTaskType::Ultraplan,
            RemoteTaskType::Ultrareview,
            RemoteTaskType::AutofixPr,
            RemoteTaskType::BackgroundPr,
        ] {
            assert_eq!(
                is_remote_task_type(Some(task_type.as_str())),
                Some(task_type)
            );
        }
        // CC :107 `v ?? ''` — undefined and unknown both fail the guard.
        assert_eq!(is_remote_task_type(None), None);
        assert_eq!(is_remote_task_type(Some("local_agent")), None);
    }

    #[test]
    fn precondition_errors_use_official_copy() {
        assert!(
            format_precondition_error(BackgroundRemoteSessionPrecondition::NotLoggedIn)
                .contains("/login")
        );
        assert!(
            format_precondition_error(BackgroundRemoteSessionPrecondition::GithubAppNotInstalled)
                .contains("https://github.com/apps/claude/installations/new")
        );
        assert!(
            format_precondition_error(BackgroundRemoteSessionPrecondition::PolicyBlocked)
                .contains("organization")
        );
    }
}

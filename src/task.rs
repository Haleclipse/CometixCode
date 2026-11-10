//! Shared task contract.
//!
//! Maps to: CC `Task.ts:1-115`.

/// Stable task type names used by AppState and SDK events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskType {
    LocalBash,
    LocalAgent,
    InProcessTeammate,
    LocalWorkflow,
    MonitorMcp,
    Dream,
}

impl TaskType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalBash => "local_bash",
            Self::LocalAgent => "local_agent",
            Self::InProcessTeammate => "in_process_teammate",
            Self::LocalWorkflow => "local_workflow",
            Self::MonitorMcp => "monitor_mcp",
            Self::Dream => "dream",
        }
    }

    const fn id_prefix(self) -> char {
        match self {
            Self::LocalBash => 'b',
            Self::LocalAgent => 'a',
            Self::InProcessTeammate => 't',
            Self::LocalWorkflow => 'w',
            Self::MonitorMcp => 'm',
            Self::Dream => 'd',
        }
    }
}

const TASK_ID_ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Generate the case-insensitive-safe 9-character task ID used by CC.
/// Maps to CC `Task.ts#generateTaskId`.
pub fn generate_task_id(task_type: TaskType) -> String {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).expect("OS randomness is required for secure task IDs");
    let mut id = String::with_capacity(9);
    id.push(task_type.id_prefix());
    for byte in random {
        id.push(TASK_ID_ALPHABET[byte as usize % TASK_ID_ALPHABET.len()] as char);
    }
    id
}

pub fn is_terminal_task_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "killed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_task_ids_use_official_prefix_length_and_alphabet() {
        for (task_type, prefix) in [
            (TaskType::LocalBash, 'b'),
            (TaskType::LocalAgent, 'a'),
            (TaskType::InProcessTeammate, 't'),
            (TaskType::LocalWorkflow, 'w'),
            (TaskType::MonitorMcp, 'm'),
            (TaskType::Dream, 'd'),
        ] {
            let id = generate_task_id(task_type);
            assert_eq!(id.len(), 9);
            assert_eq!(id.chars().next(), Some(prefix));
            assert!(
                id.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            );
        }
    }
}

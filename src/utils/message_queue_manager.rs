//! Unified command queue.
//!
//! Maps to CC `utils/messageQueueManager.ts`.
//!
//! This is the module-level store used by the iocraft queue hook, prompt editor,
//! background task notifications, and query attachment draining. It preserves
//! the official queue semantics (priority order, task-notification defaults,
//! FIFO within priority, editable/visible projections) while keeping UI state
//! ownership in the consuming components.

#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuePriority {
    Now,
    Next,
    Later,
}

impl QueuePriority {
    fn rank(self) -> u8 {
        self.rank_for_query()
    }

    /// Maps to CC `PRIORITY_ORDER` numeric ordering.
    pub fn rank_for_query(self) -> u8 {
        match self {
            Self::Now => 0,
            Self::Next => 1,
            Self::Later => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedCommand {
    pub value: String,
    /// Expanded input sent to the model; the raw value remains available for
    /// hook and keyword checks, matching CC `preExpansionValue`.
    pub pre_expansion_value: Option<String>,
    /// Pasted text/image metadata is carried until execution. Images are
    /// materialized only when the command reaches the query boundary.
    pub pasted_contents:
        BTreeMap<usize, crate::components::prompt_input::input_paste::PastedContent>,
    pub mode: String,
    pub priority: QueuePriority,
    pub agent_id: Option<String>,
    pub is_meta: bool,
    pub uuid: Option<String>,
    pub skip_slash_commands: bool,
}

impl QueuedCommand {
    pub fn new(value: impl Into<String>, mode: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            pre_expansion_value: None,
            pasted_contents: BTreeMap::new(),
            mode: mode.into(),
            priority: QueuePriority::Next,
            agent_id: None,
            is_meta: false,
            uuid: None,
            skip_slash_commands: false,
        }
    }
}

static COMMAND_QUEUE: LazyLock<Mutex<Vec<QueuedCommand>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static QUEUE_SUBSCRIBERS: LazyLock<Mutex<Vec<async_channel::Sender<()>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

#[cfg(test)]
pub static TEST_QUEUE_LOCK: LazyLock<TestStateLock> = LazyLock::new(TestStateLock::new);

fn notify_subscribers() {
    QUEUE_SUBSCRIBERS.lock().unwrap().retain(|sender| {
        if sender.is_closed() {
            return false;
        }
        // Coalesce mutations while a component has not consumed its prior
        // notification, like useSyncExternalStore's frozen snapshot update.
        let _ = sender.try_send(());
        true
    });
}

/// Subscribes to queue mutations. Dropping the receiver unregisters lazily on
/// the next notification.
pub fn subscribe_to_command_queue() -> async_channel::Receiver<()> {
    let (sender, receiver) = async_channel::bounded(1);
    QUEUE_SUBSCRIBERS.lock().unwrap().push(sender);
    receiver
}

/// Maps to CC `messageQueueManager.ts#recheckCommandQueue`.
pub fn recheck_command_queue() {
    if get_command_queue_length() > 0 {
        notify_subscribers();
    }
}

/// Maps to: CC `messageQueueManager.ts:128-135` `enqueue` — pushes the
/// command as given. CC's `priority ?? 'next'` fallback lives in the Rust
/// constructor default (`QueuedCommand::new` → `Next`); CC never invents a
/// uuid here (the REPL submit path stamps one, notification callers leave it
/// unset and the consumed-uuid bookkeeping skips them, query.ts:1637).
pub fn enqueue(command: QueuedCommand) {
    COMMAND_QUEUE.lock().unwrap().push(command);
    notify_subscribers();
    log_operation("enqueue");
}

/// Maps to CC `messageQueueManager.ts#enqueuePendingNotification`.
/// CC `priority ?? 'later'` (:143) respects an explicit caller value — the
/// live branch: LocalShellTask stamps `'next'` on interactive-prompt
/// notifications so they drain mid-turn (LocalShellTask.tsx:125-128).
/// The Rust field is non-optional, so every caller writes its CC site's
/// value explicitly; unconditionally forcing `Later` here silently rebucketed
/// those 'next' notifications. No invented uuid either (see `enqueue`).
pub fn enqueue_pending_notification(command: QueuedCommand) {
    COMMAND_QUEUE.lock().unwrap().push(command);
    notify_subscribers();
    log_operation("enqueue");
}

/// Maps to CC `messageQueueManager.ts:97-100` `getCommandQueueLength`.
pub fn get_command_queue_length() -> usize {
    COMMAND_QUEUE.lock().unwrap().len()
}

/// Maps to CC `messageQueueManager.ts:90-93` `getCommandQueue`.
pub fn get_command_queue() -> Vec<QueuedCommand> {
    COMMAND_QUEUE.lock().unwrap().clone()
}

/// Returns the current queue snapshot for reactive consumers.
///
/// Maps to CC `messageQueueManager.ts:78-81` `getCommandQueueSnapshot`. Rust cannot
/// expose a frozen JavaScript array, so the snapshot is an owned clone; the
/// subscription notification still carries the same mutation boundary.
pub fn get_command_queue_snapshot() -> Vec<QueuedCommand> {
    get_command_queue()
}

/// Maps to CC `messageQueueManager.ts:104-106` `hasCommandsInQueue`.
pub fn has_commands_in_queue() -> bool {
    get_command_queue_length() > 0
}

/// Maps to CC `messageQueueManager.ts#peek`.
pub fn peek(filter: impl Fn(&QueuedCommand) -> bool) -> Option<QueuedCommand> {
    let queue = COMMAND_QUEUE.lock().unwrap();
    let mut best: Option<&QueuedCommand> = None;
    let mut best_priority = u8::MAX;
    for command in queue.iter() {
        if !filter(command) {
            continue;
        }
        let priority = command.priority.rank();
        if priority < best_priority {
            best_priority = priority;
            best = Some(command);
        }
    }
    best.cloned()
}

/// Maps to CC `messageQueueManager.ts#dequeue`.
pub fn dequeue(filter: impl Fn(&QueuedCommand) -> bool) -> Option<QueuedCommand> {
    let mut queue = COMMAND_QUEUE.lock().unwrap();
    let mut best_idx = None;
    let mut best_priority = u8::MAX;
    for (idx, command) in queue.iter().enumerate() {
        if !filter(command) {
            continue;
        }
        let priority = command.priority.rank();
        if priority < best_priority {
            best_priority = priority;
            best_idx = Some(idx);
        }
    }
    let idx = best_idx?;
    let command = queue.remove(idx);
    drop(queue);
    notify_subscribers();
    log_operation("dequeue");
    Some(command)
}

/// Maps to CC `messageQueueManager.ts:199-212` `dequeueAll`.
pub fn dequeue_all() -> Vec<QueuedCommand> {
    let mut queue = COMMAND_QUEUE.lock().unwrap();
    if queue.is_empty() {
        return Vec::new();
    }
    let commands = std::mem::take(&mut *queue);
    drop(queue);
    notify_subscribers();
    for _ in &commands {
        log_operation("dequeue");
    }
    commands
}

/// Maps to CC `messageQueueManager.ts#dequeueAllMatching`.
pub fn dequeue_all_matching(predicate: impl Fn(&QueuedCommand) -> bool) -> Vec<QueuedCommand> {
    let mut queue = COMMAND_QUEUE.lock().unwrap();
    let mut matched = Vec::new();
    let mut remaining = Vec::new();
    for command in queue.drain(..) {
        if predicate(&command) {
            matched.push(command);
        } else {
            remaining.push(command);
        }
    }
    *queue = remaining;
    drop(queue);
    if !matched.is_empty() {
        notify_subscribers();
    }
    for _ in &matched {
        log_operation("dequeue");
    }
    matched
}

/// Maps to CC `messageQueueManager.ts:273-295` `remove`.
///
/// The JS owner removes by object identity.  Rust's queue carrier is cloned
/// at the snapshot boundary, so equality is the stable representation-level
/// equivalent; only one matching queue row is removed for each requested
/// value, preserving duplicate rows just as identity removal does.
pub fn remove(commands_to_remove: &[QueuedCommand]) {
    if commands_to_remove.is_empty() {
        return;
    }
    let mut queue = COMMAND_QUEUE.lock().unwrap();
    let before = queue.len();
    let mut removed_count = 0;
    for command in commands_to_remove {
        if let Some(index) = queue.iter().position(|candidate| candidate == command) {
            queue.remove(index);
            removed_count += 1;
        }
    }
    let changed = queue.len() != before;
    drop(queue);
    if changed {
        notify_subscribers();
    }
    for _ in 0..removed_count {
        log_operation("remove");
    }
}

/// Maps to CC `messageQueueManager.ts:298-318` `removeByFilter`.
pub fn remove_by_filter(predicate: impl Fn(&QueuedCommand) -> bool) -> Vec<QueuedCommand> {
    let mut queue = COMMAND_QUEUE.lock().unwrap();
    let mut removed = Vec::new();
    let mut retained = Vec::with_capacity(queue.len());
    for command in queue.drain(..) {
        if predicate(&command) {
            removed.push(command);
        } else {
            retained.push(command);
        }
    }
    *queue = retained;
    drop(queue);
    if !removed.is_empty() {
        notify_subscribers();
        for _ in &removed {
            log_operation("remove");
        }
    }
    removed
}

/// Maps to: CC `messageQueueManager.ts:525-532` `getCommandsByMaxPriority` —
/// a pure peek: filters the queue by priority threshold without removing.
/// CC's `PRIORITY_ORDER[cmd.priority ?? 'next']` fallback is already baked in
/// here because the Rust field is non-optional with `Next` as the constructor
/// default. Live CC consumers: the print-mode abort-on-'now' subscription
/// (cli/print.ts:1860, print interrupt surface not yet ported) and the
/// mid-turn drain peek (query.ts:1569, projected in Rust as the single-step
/// `drain_queued_commands_snapshot` in query.rs).
pub fn get_commands_by_max_priority(max_priority: QueuePriority) -> Vec<QueuedCommand> {
    let threshold = max_priority.rank_for_query();
    let queue = COMMAND_QUEUE.lock().unwrap();
    queue
        .iter()
        .filter(|command| command.priority.rank_for_query() <= threshold)
        .cloned()
        .collect()
}

/// Maps to CC `messageQueueManager.ts#clearCommandQueue`.
pub fn clear_command_queue() {
    let mut queue = COMMAND_QUEUE.lock().unwrap();
    if queue.is_empty() {
        return;
    }
    queue.clear();
    drop(queue);
    notify_subscribers();
}

/// Maps to CC `messageQueueManager.ts:334-337` `resetCommandQueue`.
///
/// The JS reset also resets its frozen snapshot.  Rust snapshots are cloned
/// on demand, so clearing the queue is the complete equivalent.
pub fn reset_command_queue() {
    clear_command_queue();
}

/// Maps to CC `messageQueueManager.ts:350-356` `isPromptInputModeEditable`.
pub fn is_prompt_input_mode_editable(mode: &str) -> bool {
    mode != "task-notification"
}

/// Maps to CC `messageQueueManager.ts:359-365` `isQueuedCommandEditable`: task notifications and meta commands
/// must never leak their raw payload into the prompt editor.
pub fn is_queued_command_editable(command: &QueuedCommand) -> bool {
    is_prompt_input_mode_editable(&command.mode) && !command.is_meta
}

/// Maps to CC `messageQueueManager.ts:368-376` `isQueuedCommandVisible`.
///
/// The current Rust carrier has no channel-origin field or feature-gated
/// channel preview.  For the represented fields, visibility is exactly the
/// editable predicate; task notifications/meta commands remain hidden from
/// the prompt preview.
pub fn is_queued_command_visible(command: &QueuedCommand) -> bool {
    is_queued_command_editable(command)
}

/// Maps to CC `messageQueueManager.ts:541-547` `isSlashCommand`.
///
/// This intentionally remains separate from `queue_processor::is_slash_command`:
/// CC has two source-defined predicates.  The manager predicate honors
/// `skipSlashCommands` for bridge/remote input; queueProcessor's private
/// batching predicate only checks the value shape.
pub fn is_slash_command(command: &QueuedCommand) -> bool {
    command.value.trim_start().starts_with('/') && !command.skip_slash_commands
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PopAllEditableResult {
    pub text: String,
    pub cursor_offset: usize,
    pub images: Vec<crate::components::prompt_input::input_paste::PastedContent>,
}

/// Maps to CC `popAllEditable(currentInput, currentCursorOffset)`.
pub fn pop_all_editable(
    current_input: &str,
    current_cursor_offset: usize,
) -> Option<PopAllEditableResult> {
    let mut queue = COMMAND_QUEUE.lock().unwrap();
    let mut editable = Vec::new();
    let mut retained = Vec::new();
    for command in queue.drain(..) {
        if is_queued_command_editable(&command) {
            editable.push(command);
        } else {
            retained.push(command);
        }
    }
    if editable.is_empty() {
        *queue = retained;
        return None;
    }
    let queued_text = editable
        .iter()
        .map(|command| command.value.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let text = [queued_text.as_str(), current_input]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let cursor_offset = if current_input.is_empty() {
        queued_text.len()
    } else {
        queued_text.len() + 1 + current_cursor_offset.min(current_input.len())
    };
    let images = editable
        .iter()
        .flat_map(|command| command.pasted_contents.values())
        .filter(|content| {
            matches!(
                content,
                crate::components::prompt_input::input_paste::PastedContent::Image {
                    data: Some(data),
                    ..
                } if !data.is_empty()
            )
        })
        .cloned()
        .collect();
    *queue = retained;
    drop(queue);
    notify_subscribers();
    for _ in &editable {
        log_operation("popAll");
    }
    Some(PopAllEditableResult {
        text,
        cursor_offset,
        images,
    })
}

// Backward-compatible aliases from CC `messageQueueManager.ts:491-516`.
// Keep these in the same owner so older consumers do not recreate a second
// pending-notification queue abstraction.
/// Deprecated alias for `subscribe_to_command_queue`.
pub fn subscribe_to_pending_notifications() -> async_channel::Receiver<()> {
    subscribe_to_command_queue()
}

/// Deprecated alias for `get_command_queue_snapshot`.
pub fn get_pending_notifications_snapshot() -> Vec<QueuedCommand> {
    get_command_queue_snapshot()
}

/// Deprecated alias for `has_commands_in_queue`.
pub fn has_pending_notifications() -> bool {
    has_commands_in_queue()
}

/// Deprecated alias for `get_command_queue_length`.
pub fn get_pending_notifications_count() -> usize {
    get_command_queue_length()
}

/// Deprecated alias for `recheck_command_queue`.
pub fn recheck_pending_notifications() {
    recheck_command_queue();
}

/// Deprecated alias for `dequeue` without a filter.
pub fn dequeue_pending_notification() -> Option<QueuedCommand> {
    dequeue(|_| true)
}

/// Deprecated alias for `reset_command_queue`.
pub fn reset_pending_notifications() {
    reset_command_queue();
}

/// Deprecated alias for `clear_command_queue`.
pub fn clear_pending_notifications() {
    clear_command_queue();
}

fn log_operation(_operation: &str) {
    // Maps to CC `logOperation(...)` -> `recordQueueOperation(...)`.
    // Queue-operation persistence remains an explicit seam even though the
    // main session transcript writer is live.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pop_all_editable_preserves_notifications_and_combines_prompt_text() {
        let _lock = TEST_QUEUE_LOCK.lock().unwrap();
        clear_command_queue();
        enqueue(QueuedCommand::new("queued one", "prompt"));
        let mut notification = QueuedCommand::new("<task>", "task-notification");
        notification.is_meta = true;
        enqueue_pending_notification(notification);
        enqueue(QueuedCommand::new("queued two", "bash"));

        let result = pop_all_editable("draft", 2).expect("editable queue");
        assert_eq!(result.text, "queued one\nqueued two\ndraft");
        assert_eq!(result.cursor_offset, "queued one\nqueued two\n".len() + 2);
        assert_eq!(get_command_queue().len(), 1);
        assert_eq!(get_command_queue()[0].mode, "task-notification");
        clear_command_queue();
    }

    #[test]
    fn queue_dequeues_by_priority_then_fifo_like_official() {
        let _lock = TEST_QUEUE_LOCK.lock().unwrap();
        clear_command_queue();
        let mut later = QueuedCommand::new("later", "task-notification");
        later.priority = QueuePriority::Later;
        enqueue_pending_notification(later);
        enqueue(QueuedCommand::new("next-1", "prompt"));
        enqueue(QueuedCommand::new("next-2", "prompt"));
        let mut now = QueuedCommand::new("now", "prompt");
        now.priority = QueuePriority::Now;
        enqueue(now);

        assert_eq!(dequeue(|_| true).unwrap().value, "now");
        assert_eq!(dequeue(|_| true).unwrap().value, "next-1");
        assert_eq!(dequeue(|_| true).unwrap().value, "next-2");
        assert_eq!(dequeue(|_| true).unwrap().value, "later");
        assert!(dequeue(|_| true).is_none());
    }

    #[test]
    fn manager_slash_predicate_honors_bridge_skip_flag() {
        let plain = QueuedCommand::new("  /help", "prompt");
        assert!(is_slash_command(&plain));
        let mut bridge = QueuedCommand::new("/help", "prompt");
        bridge.skip_slash_commands = true;
        assert!(!is_slash_command(&bridge));
    }

    #[test]
    fn pop_all_editable_restores_queued_images() {
        let _lock = TEST_QUEUE_LOCK.lock().unwrap();
        clear_command_queue();
        enqueue(QueuedCommand {
            value: "[Image #4]".to_string(),
            pre_expansion_value: Some("[Image #4]".to_string()),
            pasted_contents: std::collections::BTreeMap::from([(
                4,
                crate::components::prompt_input::input_paste::PastedContent::Image {
                    id: 4,
                    media_type: Some("image/png".to_string()),
                    data: Some("AAAA".to_string()),
                    filename: None,
                    dimensions: None,
                    source_path: None,
                },
            )]),
            mode: "prompt".to_string(),
            priority: QueuePriority::Next,
            agent_id: None,
            is_meta: false,
            uuid: None,
            skip_slash_commands: false,
        });
        let result = pop_all_editable("", 0).expect("editable queue");
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].id(), 4);
        clear_command_queue();
    }
}

//! @nyan: `/retrieval` maintenance commands (docs/nyan/retrieval.md 4.8).
//!
//! `/retrieval prune [--dry-run]` lists this machine's Qdrant collections that
//! are either replaced by the current generation or have not synced for
//! `sync.pruneAfterDays`, and deletes them unless `--dry-run` is passed. Only
//! collections recorded in this worktree's ledger under this machine's
//! `worktreeId` prefix are ever touched; another machine's collections are
//! cleaned on that machine.
//!
//! `/retrieval status` prints the index generation, semantic generation,
//! collection name and pending-file count without contacting any service.
//!
//! `/retrieval sync` starts a resumable semantic sync in the process runtime;
//! add `--wait` when a caller needs the final report before returning.

use crate::commands::Command;
use crate::utils::process_user_input::ProcessUserInputBaseResult;
use crate::utils::process_user_input::process_slash_command::local_command_result;

pub const NAME: &str = "retrieval";
pub const DESCRIPTION: &str = "Inspect, sync, or prune the local and remote retrieval indexes";

pub fn command() -> Command {
    Command::local(NAME, DESCRIPTION)
        .argument_hint("<status|sync [--wait]|prune [--dry-run]>")
        .supports_non_interactive()
        .executable(call)
}

/// Maps to: retrieval.md 4.8 nyan-ops collection cleanup.
pub fn call(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    let (output, is_error) = match run(args, context) {
        Ok(output) => (output, false),
        Err(error) => (error, true),
    };
    local_command_result(
        uuid,
        crate::commands::get_command_name(command),
        args,
        output,
        is_error,
        None,
    )
}

fn run(args: &str, context: &crate::tool::ToolUseContext) -> Result<String, String> {
    let mut parts = args.split_whitespace();
    let subcommand = parts.next().unwrap_or("status");
    let flags = parts.collect::<Vec<_>>();
    match subcommand {
        "status" => status(context),
        "sync" => sync(context, flags.contains(&"--wait")),
        "prune" => prune(context, flags.contains(&"--dry-run")),
        other => Err(format!(
            "unknown /retrieval subcommand {other}; use status, sync [--wait], or prune"
        )),
    }
}

fn status(context: &crate::tool::ToolUseContext) -> Result<String, String> {
    let root = context
        .effective_cwd()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let settings = crate::utils::settings::get_settings_with_errors().settings;
    let retrieval = settings
        .nyan
        .as_ref()
        .map(|nyan| nyan.retrieval())
        .unwrap_or_default();
    if !retrieval.enabled() {
        return Ok("Retrieval is disabled (nyan.retrieval.enabled = false).".to_string());
    }
    let index = crate::services::retrieval::index::LocalIndex::open(
        &root,
        retrieval.index().max_file_bytes(),
        &context.tool_permission_context,
    )?;
    let mut lines = Vec::new();
    lines.push(format!("worktree: {}", root.display()));
    lines.push(format!(
        "index generation: {} ({} files, {} skipped, {:?})",
        index.generation,
        index.files().len(),
        index.skipped.total(),
        index.state
    ));
    if index.skipped.total() > 0 {
        lines.push(format!(
            "skipped: {} binary, {} too large, {} sensitive, {} denied, {} unsupported",
            index.skipped.binary,
            index.skipped.too_large,
            index.skipped.sensitive,
            index.skipped.denied,
            index.skipped.unsupported
        ));
    }
    match retrieval.semantic() {
        Some(semantic) => {
            let budget = retrieval.budget();
            let client = crate::services::retrieval::semantic::SemanticClient::from_settings(
                &semantic, &root, &budget,
            )
            .map_err(|error| error.to_string())?;
            lines.push(format!("semantic generation: {}", client.generation()));
            lines.push(format!("collection: {}", client.collection()));
            lines.push(format!(
                "pending files: {}",
                crate::services::retrieval::semantic::SemanticClient::pending_count(&index)
            ));
        }
        None => lines.push("semantic: not configured".to_string()),
    }
    Ok(lines.join("\n"))
}

fn sync(context: &crate::tool::ToolUseContext, wait: bool) -> Result<String, String> {
    let root = context
        .effective_cwd()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let settings = crate::utils::settings::get_settings_with_errors().settings;
    let retrieval = settings
        .nyan
        .as_ref()
        .map(|nyan| nyan.retrieval())
        .unwrap_or_default();
    let Some(semantic) = retrieval.semantic() else {
        return Err("semantic retrieval is not configured; nothing to sync".to_string());
    };
    let budget = retrieval.budget();
    let sync_settings = retrieval.sync();
    let client = crate::services::retrieval::semantic::SemanticClient::from_settings(
        &semantic, &root, &budget,
    )
    .map_err(|error| error.to_string())?;
    let index = crate::services::retrieval::index::LocalIndex::open(
        &root,
        retrieval.index().max_file_bytes(),
        &context.tool_permission_context,
    )?;
    client.queue_changed(&index)?;
    client.ensure_initial_pending(&index)?;
    let pending = crate::services::retrieval::semantic::SemanticClient::pending_count(&index);
    if pending == 0 {
        return Ok("Semantic index is already synchronized.".to_string());
    }

    if wait {
        let report = crate::utils::process_runtime::block_on_from_sync(async move {
            client.sync_pending(&index, &sync_settings).await
        })
        .ok_or_else(|| "no runtime available to run semantic sync".to_string())?;
        return Ok(format_sync_report(report));
    }

    let handle =
        crate::utils::process_runtime::runtime_handle_for_detached_work().ok_or_else(|| {
            "no process runtime available for background semantic sync; use --wait".to_string()
        })?;
    handle.spawn(async move {
        let report = client.sync_pending(&index, &sync_settings).await;
        crate::utils::debug::log_for_debugging(&format!(
            "retrieval semantic sync command: synced_chunks={} pending_files={} status={:?}",
            report.synced_chunks, report.pending_files, report.status
        ));
    });
    Ok(format!(
        "Semantic sync started in the background ({pending} files pending). Use /retrieval status to inspect progress."
    ))
}

fn format_sync_report(report: crate::services::retrieval::semantic::SyncReport) -> String {
    format!(
        "Semantic sync finished: {} chunks synced, {} files pending, status {:?}.",
        report.synced_chunks, report.pending_files, report.status
    )
}

fn prune(context: &crate::tool::ToolUseContext, dry_run: bool) -> Result<String, String> {
    let root = context
        .effective_cwd()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let settings = crate::utils::settings::get_settings_with_errors().settings;
    let retrieval = settings
        .nyan
        .as_ref()
        .map(|nyan| nyan.retrieval())
        .unwrap_or_default();
    let Some(semantic) = retrieval.semantic() else {
        return Err("semantic retrieval is not configured; nothing to prune".to_string());
    };
    let budget = retrieval.budget();
    let sync = retrieval.sync();
    let client = crate::services::retrieval::semantic::SemanticClient::from_settings(
        &semantic, &root, &budget,
    )
    .map_err(|error| error.to_string())?;
    let index = crate::services::retrieval::index::LocalIndex::open(
        &root,
        retrieval.index().max_file_bytes(),
        &context.tool_permission_context,
    )?;
    let client_for_task = client.clone();
    let index_for_task = index.clone();
    let report = crate::utils::process_runtime::block_on_from_sync(async move {
        client_for_task.prune(&index_for_task, &sync, dry_run).await
    })
    .ok_or_else(|| "no runtime available to run prune".to_string())??;
    let mut lines = Vec::new();
    if report.deleted.is_empty() {
        lines.push("No collections to prune.".to_string());
    } else if report.dry_run {
        lines.push(format!(
            "Would delete {} collection(s) (dry run):",
            report.deleted.len()
        ));
        lines.extend(report.deleted.iter().map(|name| format!("  {name}")));
    } else {
        lines.push(format!("Deleted {} collection(s):", report.deleted.len()));
        lines.extend(report.deleted.iter().map(|name| format!("  {name}")));
    }
    if !report.kept.is_empty() {
        lines.push(format!("Kept {} collection(s).", report.kept.len()));
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_subcommand_is_a_named_error() {
        let context = crate::tool::ToolUseContext::default();
        let error = run("explode", &context).unwrap_err();
        assert!(error.contains("unknown /retrieval subcommand"));
        assert!(error.contains("sync [--wait]"));
    }

    #[test]
    fn sync_without_semantic_configuration_says_so() {
        let context = crate::tool::ToolUseContext::default();
        let error = run("sync --wait", &context).unwrap_err();
        assert!(error.contains("not configured"));
    }

    #[test]
    fn prune_without_semantic_configuration_says_so() {
        let context = crate::tool::ToolUseContext::default();
        // Settings in the isolated test home have no semantic section.
        let error = run("prune --dry-run", &context).unwrap_err();
        assert!(error.contains("not configured") || error.contains("unavailable"));
    }
}

use crate::tasks::spawn_blocking;
use crate::worker::Environment;
use crate::worker::jobs::ArchiveIndexBranch;
use chrono::Utc;
use crates_io_worker::BackgroundJob;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, instrument, warn};

#[derive(Serialize, Deserialize)]
pub struct SquashIndex;

impl BackgroundJob for SquashIndex {
    const JOB_NAME: &'static str = "squash_index";
    const DEDUPLICATED: bool = true;
    const QUEUE: &'static str = "repository";

    type Context = Arc<Environment>;

    /// Collapse the index into a single commit, archiving the current history in a snapshot branch.
    #[instrument(skip_all)]
    async fn run(&self, env: Self::Context) -> anyhow::Result<()> {
        info!("Squashing the index into a single commit");

        let env_for_blocking = env.clone();
        let snapshot_branch = spawn_blocking(move || {
            let repo = env_for_blocking.lock_index()?;

            let now = Utc::now().format("%F");
            let snapshot_branch = format!("snapshot-{now}");

            let original_head = repo.head_oid()?;
            info!("Read original HEAD: {original_head}");

            let msg = format!("Collapse index into one commit\n\n\
            Previous HEAD was {original_head}, now on the `{snapshot_branch}` branch\n\n\
            More information about this change can be found [online] and on [this issue].\n\n\
            [online]: https://internals.rust-lang.org/t/cargos-crate-index-upcoming-squash-into-one-commit/8440\n\
            [this issue]: https://github.com/rust-lang/crates-io-cargo-teams/issues/47");

            let squash_start = Instant::now();
            repo.squash_to_single_commit(&msg)?;
            let new_head = repo.head_oid()?;
            info!(
                duration = squash_start.elapsed().as_nanos(),
                "Squash commit created: {new_head}",
            );

            // Shell out to git because libgit2 does not currently support push leases

            info!("Pushing squashed index to origin");
            let push_start = Instant::now();
            repo.run_command(Command::new("git").args([
                "push",
                // Both updates should succeed or fail together
                "--atomic",
                "origin",
                // Overwrite master, but only if it server matches the expected value
                &format!("--force-with-lease=refs/heads/master:{original_head}"),
                // The new squashed commit is pushed to master
                "HEAD:refs/heads/master",
                // The previous value of HEAD is pushed to a snapshot branch
                &format!("{original_head}:refs/heads/{snapshot_branch}"),
            ]))?;
            info!(
                duration = push_start.elapsed().as_nanos(),
                "Squashed index pushed to origin",
            );

            info!("The index has been successfully squashed.");
            Ok::<_, anyhow::Error>(snapshot_branch)
        })
        .await??;

        if let Err(error) = enqueue_archive_job(&env, &snapshot_branch).await {
            warn!("Failed to enqueue `ArchiveIndexBranch` job for `{snapshot_branch}`: {error}");
        }

        Ok(())
    }
}

async fn enqueue_archive_job(env: &Environment, branch: &str) -> anyhow::Result<()> {
    let conn = env.deadpool.get().await?;
    ArchiveIndexBranch::new(branch).enqueue(&conn).await?;
    Ok(())
}

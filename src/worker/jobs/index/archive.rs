use crate::tasks::spawn_blocking;
use crate::worker::Environment;
use crates_io_worker::BackgroundJob;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::Arc;
use tracing::{info, instrument};

#[derive(Serialize, Deserialize)]
pub struct ArchiveIndexBranch {
    branch: String,
}

impl ArchiveIndexBranch {
    pub fn new(branch: impl Into<String>) -> Self {
        Self {
            branch: branch.into(),
        }
    }
}

impl BackgroundJob for ArchiveIndexBranch {
    const JOB_NAME: &'static str = "archive_index_branch";
    const DEDUPLICATED: bool = true;
    const QUEUE: &'static str = "repository";

    type Context = Arc<Environment>;

    /// Mirror a snapshot branch from the crate index to the configured archive
    /// repository. No-op when no archive URL is configured.
    #[instrument(skip_all, fields(branch = self.branch))]
    async fn run(&self, env: Self::Context) -> anyhow::Result<()> {
        let Some(archive_url) = env.config.index_archive_url.clone() else {
            info!("`index_archive_url` not configured, skipping archive push");
            return Ok(());
        };

        info!(%archive_url, "Pushing snapshot to archive repository");

        let branch = self.branch.clone();
        spawn_blocking(move || {
            let repo = env.lock_index()?;

            repo.run_command(Command::new("git").args(["fetch", "origin", &branch]))?;

            repo.run_command(Command::new("git").args([
                "push",
                archive_url.as_str(),
                &format!("FETCH_HEAD:refs/heads/{branch}"),
            ]))?;

            info!("Snapshot pushed to archive repository.");
            Ok(())
        })
        .await?
    }
}

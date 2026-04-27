use crate::tasks::spawn_blocking;
use crate::worker::Environment;
use anyhow::anyhow;
use crates_io_worker::BackgroundJob;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::Arc;
use tracing::{info, instrument, warn};
use url::Url;

const REMOTE_NAME: &str = "archive";

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

        let Some(github_app) = env.github_app.as_ref() else {
            return Err(anyhow!(
                "`index_archive_url` is set but GitHub App is not configured"
            ));
        };
        let github_app = github_app.clone();

        info!(%archive_url, "Pushing snapshot to archive repository");

        let branch = self.branch.clone();
        let handle = tokio::runtime::Handle::current();

        spawn_blocking(move || {
            let repo = env.lock_index()?;

            repo.run_command(Command::new("git").args(["fetch", "origin", &branch]))?;

            let token = handle.block_on(github_app.installation_token())?;
            let push_url = match build_credentialed_url(&archive_url, token.expose_secret()) {
                Ok(url) => url,
                Err(()) => {
                    warn!(%archive_url, "Archive URL does not support credentials; pushing without auth");
                    archive_url.clone()
                }
            };

            let _remote = repo.add_temporary_remote(REMOTE_NAME, &push_url)?;
            repo.run_command(Command::new("git").args([
                "push",
                REMOTE_NAME,
                &format!("FETCH_HEAD:refs/heads/{branch}"),
            ]))?;

            info!("Snapshot pushed to archive repository.");
            Ok(())
        })
        .await?
    }
}

/// Return a copy of `base` with `x-access-token` / `token` embedded as the
/// HTTPS credentials git consumes when pushing. Returns `Err(())` when the
/// URL scheme does not allow userinfo (e.g. `file://`).
fn build_credentialed_url(base: &Url, token: &str) -> Result<Url, ()> {
    let mut url = base.clone();
    url.set_username("x-access-token")?;
    url.set_password(Some(token))?;
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use claims::assert_err;
    use insta::assert_snapshot;

    #[test]
    fn build_credentialed_url_https() {
        let url: Url = "https://github.com/rust-lang/archive.git".parse().unwrap();
        let credentialed = build_credentialed_url(&url, "tok").unwrap();
        assert_snapshot!(credentialed, @"https://x-access-token:tok@github.com/rust-lang/archive.git");
    }

    #[test]
    fn build_credentialed_url_file_rejected() {
        let url: Url = "file:///tmp/archive".parse().unwrap();
        assert_err!(build_credentialed_url(&url, "tok"));
    }
}

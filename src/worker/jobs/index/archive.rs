use crate::tasks::spawn_blocking;
use crate::worker::Environment;
use anyhow::anyhow;
use crates_io_worker::BackgroundJob;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tokio::process::Command;
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
    ///
    /// Each invocation works against a fresh, ephemeral bare clone of the
    /// snapshot branch in a `TempDir`. The job does not share state with the
    /// long-lived bare clone behind `Environment::lock_index()`.
    #[instrument(skip_all, fields(branch = self.branch))]
    async fn run(&self, env: Self::Context) -> anyhow::Result<()> {
        let Some(archive_url) = env.config.index_archive_url.as_ref() else {
            info!("`index_archive_url` not configured, skipping archive push");
            return Ok(());
        };

        let Some(github_app) = env.github_app.as_ref() else {
            let error = anyhow!("`index_archive_url` is set but GitHub App is not configured");
            return Err(error);
        };

        info!(
            "Cloning snapshot branch ({branch}) from the index repository",
            branch = self.branch
        );

        // `TempDir` create/drop are sync filesystem I/O. The bare clone is one
        // large packfile plus a handful of small refs/config files, so
        // `remove_dir_all` cost is bounded by inode count, not pack size, and
        // should stay brief enough to run on the async runtime.
        let tempdir = tempfile::Builder::new()
            .prefix("snapshot-clone")
            .tempdir()?;

        let clone_start = Instant::now();
        let output = Command::new("git")
            .args([
                "clone",
                "--bare",
                "--single-branch",
                "--branch",
                &self.branch,
                env.repository_config.index_location.as_str(),
            ])
            // `tempdir.path()` is `&Path`, so it can't share the `&str` array above.
            .arg(tempdir.path())
            .output()
            .await?;

        if !output.status.success() {
            return Err(anyhow!(
                "git clone failed: {}{}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            ));
        }

        info!(
            duration = clone_start.elapsed().as_nanos(),
            "Cloned snapshot branch ({branch})",
            branch = self.branch,
        );

        let token = github_app.installation_token().await?;
        let push_url = match build_credentialed_url(archive_url, token.expose_secret()) {
            Ok(url) => url,
            Err(()) => {
                warn!(
                    "Archive URL ({archive_url}) does not support credentials; pushing without auth"
                );
                archive_url.clone()
            }
        };

        let bare_path = tempdir.path().to_owned();
        spawn_blocking(move || -> anyhow::Result<()> {
            // Use `git2` here so the credentialed URL is written only into the
            // tempdir's `.git/config` and never appears in process argv or logs.
            let repo = git2::Repository::open_bare(&bare_path)?;
            repo.remote(REMOTE_NAME, push_url.as_str())?;
            Ok(())
        })
        .await??;
        info!("Added archive repository as `{REMOTE_NAME}` remote");

        info!(
            "Pushing snapshot branch ({branch}) to archive repository ({archive_url})",
            branch = self.branch
        );
        let refspec = format!("{branch}:refs/heads/{branch}", branch = self.branch);
        let push_start = Instant::now();
        let output = Command::new("git")
            .current_dir(tempdir.path())
            .args(["push", REMOTE_NAME, &refspec])
            .output()
            .await?;

        if !output.status.success() {
            return Err(anyhow!(
                "git push failed: {}{}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            ));
        }

        info!(
            duration = push_start.elapsed().as_nanos(),
            "Snapshot pushed to archive repository"
        );

        Ok(())
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

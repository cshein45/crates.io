use anyhow::Context;
use clap::Parser;
use crates_io_github_app::{GitHubApp, GitHubAppClient};
use secrecy::{ExposeSecret, SecretString};
use url::Url;

#[derive(Debug, Parser)]
#[command(about = "Prints a fresh installation access token for the configured GitHub App.")]
struct Opts {
    #[arg(long, env = "GH_INDEX_SYNC_APP_CLIENT_ID")]
    client_id: String,

    #[arg(long, env = "GH_INDEX_SYNC_APP_PRIVATE_KEY", hide_env_values = true)]
    private_key: SecretString,

    #[arg(long, env = "GIT_ARCHIVE_REPO_URL")]
    archive_url: Url,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();

    let org = opts
        .archive_url
        .path_segments()
        .and_then(|mut segments| segments.next())
        .filter(|segment| !segment.is_empty())
        .context("archive URL is missing the org path segment")?;

    let app = GitHubAppClient::new(&opts.client_id, opts.private_key.expose_secret(), org)?;
    let token = app.installation_token().await?;
    println!("{}", token.expose_secret());
    Ok(())
}

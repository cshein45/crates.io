#![doc = include_str!("../README.md")]

#[macro_use]
extern crate tracing;

mod slug;

pub use crate::slug::{ParseSlugError, parse_github_slug};

use oauth2::AccessToken;
use reqwest::{self, RequestBuilder, header};

use serde::de::DeserializeOwned;

use std::str;

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use url::Url;

type Result<T> = std::result::Result<T, GitHubError>;

#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait GitHubClient: Send + Sync {
    async fn current_user(&self, auth: &AccessToken) -> Result<GitHubUser>;
    async fn get_user(&self, name: &str, auth: &AccessToken) -> Result<GitHubUser>;
    async fn org_by_name(&self, org_name: &str, auth: &AccessToken) -> Result<GitHubOrganization>;
    async fn team_by_name(
        &self,
        org_name: &str,
        team_name: &str,
        auth: &AccessToken,
    ) -> Result<GitHubTeam>;
    async fn team_membership(
        &self,
        org_id: i32,
        team_id: i32,
        username: &str,
        auth: &AccessToken,
    ) -> Result<Option<GitHubTeamMembership>>;
    async fn org_membership(
        &self,
        org_id: i32,
        username: &str,
        auth: &AccessToken,
    ) -> Result<Option<GitHubOrgMembership>>;
    async fn public_keys(&self, username: &str, password: &str) -> Result<Vec<GitHubPublicKey>>;

    /// Fetches a single git ref.
    ///
    /// `ref_name` may be given either fully qualified (e.g.
    /// `"refs/heads/master"`) or without the `refs/` prefix (e.g.
    /// `"heads/master"`). The call is unauthenticated; the crates.io
    /// index repositories are public and the 60/hour unauthenticated
    /// rate limit is plenty for this use case.
    async fn get_ref(&self, owner: &str, repo: &str, ref_name: &str) -> Result<GitRef>;

    /// Fetches a single commit object by its SHA.
    ///
    /// Unauthenticated, same rationale as [`GitHubClient::get_ref`].
    async fn get_commit(&self, owner: &str, repo: &str, sha: &str) -> Result<GitCommit>;
}

#[derive(Debug)]
pub struct RealGitHubClient {
    client: Client,
    base_url: Url,
}

impl RealGitHubClient {
    pub fn new(client: Client) -> Self {
        let base_url = Url::parse("https://api.github.com").expect("base URL must parse");
        Self::with_base_url(client, base_url)
    }

    fn with_base_url(client: Client, base_url: Url) -> Self {
        Self { client, base_url }
    }

    /// Does all the nonsense for sending a GET to GitHub.
    async fn _request<T, A>(&self, url: &str, apply_auth: A) -> Result<T>
    where
        T: DeserializeOwned,
        A: Fn(RequestBuilder) -> RequestBuilder,
    {
        let url = self
            .base_url
            .join(url.trim_start_matches('/'))
            .map_err(|e| GitHubError::Other(e.into()))?;
        info!("GitHub request: GET {url}");

        let request = self
            .client
            .get(url)
            .header(header::ACCEPT, "application/vnd.github.v3+json")
            .header(header::USER_AGENT, "crates.io (https://crates.io)");

        let response = apply_auth(request).send().await?.error_for_status()?;

        let headers = response.headers();
        let remaining = headers.get("x-ratelimit-remaining");
        let limit = headers.get("x-ratelimit-limit");
        debug!("GitHub rate limit remaining: {remaining:?}/{limit:?}");

        response.json().await.map_err(Into::into)
    }

    /// Sends a GET to GitHub using OAuth access token authentication
    pub async fn request<T>(&self, url: &str, auth: &AccessToken) -> Result<T>
    where
        T: DeserializeOwned,
    {
        self._request(url, |r| r.bearer_auth(auth.secret())).await
    }

    /// Sends a GET to GitHub using basic authentication
    pub async fn request_basic<T>(&self, url: &str, username: &str, password: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        self._request(url, |r| r.basic_auth(username, Some(password)))
            .await
    }
}

#[async_trait]
impl GitHubClient for RealGitHubClient {
    async fn current_user(&self, auth: &AccessToken) -> Result<GitHubUser> {
        self.request("/user", auth).await
    }

    async fn get_user(&self, name: &str, auth: &AccessToken) -> Result<GitHubUser> {
        let url = format!("/users/{name}");
        self.request(&url, auth).await
    }

    async fn org_by_name(&self, org_name: &str, auth: &AccessToken) -> Result<GitHubOrganization> {
        let url = format!("/orgs/{org_name}");
        self.request(&url, auth).await
    }

    async fn team_by_name(
        &self,
        org_name: &str,
        team_name: &str,
        auth: &AccessToken,
    ) -> Result<GitHubTeam> {
        let url = format!("/orgs/{org_name}/teams/{team_name}");
        self.request(&url, auth).await
    }

    async fn team_membership(
        &self,
        org_id: i32,
        team_id: i32,
        username: &str,
        auth: &AccessToken,
    ) -> Result<Option<GitHubTeamMembership>> {
        let url = format!("/organizations/{org_id}/team/{team_id}/memberships/{username}");
        match self.request(&url, auth).await {
            Ok(membership) => Ok(Some(membership)),
            // Officially how `false` is returned
            Err(GitHubError::NotFound(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    async fn org_membership(
        &self,
        org_id: i32,
        username: &str,
        auth: &AccessToken,
    ) -> Result<Option<GitHubOrgMembership>> {
        let url = format!("/organizations/{org_id}/memberships/{username}");
        match self.request(&url, auth).await {
            Ok(membership) => Ok(Some(membership)),
            Err(GitHubError::NotFound(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Returns the list of public keys that can be used to verify GitHub secret alert signatures
    async fn public_keys(&self, username: &str, password: &str) -> Result<Vec<GitHubPublicKey>> {
        let url = "/meta/public_keys/secret_scanning";
        match self
            .request_basic::<GitHubPublicKeyList>(url, username, password)
            .await
        {
            Ok(v) => Ok(v.public_keys),
            Err(e) => Err(e),
        }
    }

    async fn get_ref(&self, owner: &str, repo: &str, ref_name: &str) -> Result<GitRef> {
        let ref_path = ref_name.strip_prefix("refs/").unwrap_or(ref_name);
        let path = format!("/repos/{owner}/{repo}/git/ref/{ref_path}");
        self._request(&path, std::convert::identity).await
    }

    async fn get_commit(&self, owner: &str, repo: &str, sha: &str) -> Result<GitCommit> {
        let path = format!("/repos/{owner}/{repo}/git/commits/{sha}");
        self._request(&path, std::convert::identity).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    #[error(transparent)]
    Unauthorized(anyhow::Error),
    #[error(transparent)]
    Forbidden(anyhow::Error),
    #[error(transparent)]
    NotFound(anyhow::Error),
    #[error(transparent)]
    Other(anyhow::Error),
}

impl From<reqwest::Error> for GitHubError {
    fn from(error: reqwest::Error) -> Self {
        use reqwest::StatusCode as Status;

        match error.status() {
            Some(Status::UNAUTHORIZED) => Self::Unauthorized(error.into()),
            Some(Status::FORBIDDEN) => Self::Forbidden(error.into()),
            Some(Status::NOT_FOUND) => Self::NotFound(error.into()),
            _ => Self::Other(error.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct GitHubUser {
    pub avatar_url: Option<String>,
    pub email: Option<String>,
    pub id: i32,
    pub login: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GitHubOrganization {
    pub id: i32, // unique GH id (needed for membership queries)
    pub avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GitHubTeam {
    pub id: i32,              // unique GH id (needed for membership queries)
    pub name: Option<String>, // Pretty name
    pub organization: GitHubOrganization,
}

#[derive(Debug, Deserialize)]
pub struct GitHubTeamMembership {
    pub state: String,
}

impl GitHubTeamMembership {
    pub fn is_active(&self) -> bool {
        self.state == "active"
    }
}

#[derive(Debug, Deserialize)]
pub struct GitHubOrgMembership {
    pub state: String,
    pub role: String,
}

impl GitHubOrgMembership {
    pub fn is_active_admin(&self) -> bool {
        self.state == "active" && self.role == "admin"
    }
}

#[derive(Debug, Deserialize, Clone, Eq, Hash, PartialEq)]
pub struct GitHubPublicKey {
    pub key_identifier: String,
    pub key: String,
    pub is_current: bool,
}

#[derive(Debug, Deserialize)]
pub struct GitHubPublicKeyList {
    pub public_keys: Vec<GitHubPublicKey>,
}

/// A git ref on GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GitRef {
    /// The fully qualified ref name (e.g. `"refs/heads/master"`).
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub object: GitObject,
}

/// A git object referenced from a ref or commit.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GitObject {
    pub sha: String,
}

/// A git commit on GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GitCommit {
    pub sha: String,
    pub tree: GitObject,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::{Server, ServerOpts};

    async fn mock_server() -> Server {
        Server::new_with_opts_async(ServerOpts {
            assert_on_drop: true,
            ..Default::default()
        })
        .await
    }

    fn client_with_server(server: &Server) -> RealGitHubClient {
        let base_url = Url::parse(&server.url()).unwrap();
        RealGitHubClient::with_base_url(Client::new(), base_url)
    }

    const USER_BODY: &str = r#"{
        "avatar_url": "https://avatars.githubusercontent.com/u/1?v=4",
        "email": null,
        "id": 1,
        "login": "johnnydee",
        "name": "John Doe"
    }"#;

    const REF_BODY: &str = r#"{
        "ref": "refs/heads/master",
        "node_id": "abc",
        "url": "https://api.github.com/ignored",
        "object": {
            "type": "commit",
            "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "url": "https://api.github.com/ignored"
        }
    }"#;

    const COMMIT_BODY: &str = r#"{
        "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "node_id": "abc",
        "url": "https://api.github.com/ignored",
        "html_url": "https://github.com/ignored",
        "author": {"name": "bors", "email": "bors@rust-lang.org", "date": "2026-04-24T00:00:00Z"},
        "committer": {"name": "bors", "email": "bors@rust-lang.org", "date": "2026-04-24T00:00:00Z"},
        "tree": {
            "sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "url": "https://api.github.com/ignored"
        },
        "message": "ignored",
        "parents": []
    }"#;

    #[tokio::test]
    async fn get_user_hits_configured_base_url() {
        let mut server = mock_server().await;
        let _mock = server
            .mock("GET", "/users/johnnydee")
            .match_header("authorization", "Bearer test-token")
            .with_status(200)
            .with_body(USER_BODY)
            .expect(1)
            .create_async()
            .await;

        let client = client_with_server(&server);
        let auth = AccessToken::new("test-token".into());
        let user = client.get_user("johnnydee", &auth).await.unwrap();

        assert_eq!(user.login, "johnnydee");
        assert_eq!(user.id, 1);
    }

    #[tokio::test]
    async fn get_ref_strips_refs_prefix_and_returns_sha() {
        let mut server = mock_server().await;
        let _mock = server
            .mock(
                "GET",
                "/repos/rust-lang/crates.io-index/git/ref/heads/master",
            )
            .match_header("accept", "application/vnd.github.v3+json")
            .match_header("user-agent", "crates.io (https://crates.io)")
            .with_status(200)
            .with_body(REF_BODY)
            .expect(1)
            .create_async()
            .await;

        let client = client_with_server(&server);
        let got = client
            .get_ref("rust-lang", "crates.io-index", "refs/heads/master")
            .await
            .unwrap();

        assert_eq!(got.ref_name, "refs/heads/master");
        assert_eq!(got.object.sha, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    }

    #[tokio::test]
    async fn get_ref_accepts_unqualified_ref_name() {
        let mut server = mock_server().await;
        let _mock = server
            .mock(
                "GET",
                "/repos/rust-lang/crates.io-index/git/ref/heads/master",
            )
            .with_status(200)
            .with_body(REF_BODY)
            .expect(1)
            .create_async()
            .await;

        let client = client_with_server(&server);
        let got = client
            .get_ref("rust-lang", "crates.io-index", "heads/master")
            .await
            .unwrap();

        assert_eq!(got.ref_name, "refs/heads/master");
    }

    #[tokio::test]
    async fn get_commit_returns_sha_and_tree_sha() {
        let mut server = mock_server().await;
        let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let _mock = server
            .mock(
                "GET",
                format!("/repos/rust-lang/crates.io-index/git/commits/{sha}").as_str(),
            )
            .match_header("accept", "application/vnd.github.v3+json")
            .with_status(200)
            .with_body(COMMIT_BODY)
            .expect(1)
            .create_async()
            .await;

        let client = client_with_server(&server);
        let got = client
            .get_commit("rust-lang", "crates.io-index", sha)
            .await
            .unwrap();

        assert_eq!(got.sha, sha);
        assert_eq!(got.tree.sha, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    }
}

use crate::util::TestApp;
use chrono::Utc;
use claims::assert_ok;
use crates_io::schema::background_jobs;
use crates_io::worker::jobs;
use crates_io_github::{GitCommit, GitObject, GitRef, MockGitHubClient};
use crates_io_worker::BackgroundJob;
use diesel_async::RunQueryDsl;
use url::Url;

const OWNER: &str = "rust-lang";
const REPO: &str = "crates.io-index";
const MASTER_REF: &str = "refs/heads/master";
const ORIGINAL_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TREE_SHA: &str = "ffffffffffffffffffffffffffffffffffffffff";
const NEW_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn index_url() -> Url {
    format!("https://github.com/{OWNER}/{REPO}.git")
        .parse()
        .unwrap()
}

fn master_ref(sha: &str) -> GitRef {
    GitRef {
        ref_name: MASTER_REF.into(),
        object: GitObject { sha: sha.into() },
    }
}

/// `SquashIndex` should collapse the upstream index into a single parentless
/// commit on `master`, while preserving tree content and archiving the previous
/// HEAD on a `snapshot-<date>` branch.
#[tokio::test(flavor = "multi_thread")]
async fn squash_index() {
    let (app, _) = TestApp::full().empty().await;
    let conn = app.db_conn().await;
    let upstream = app.upstream_index();

    // Seed a couple of entries so the squash has real content to collapse.
    upstream.write_file("1/a", "a\n").unwrap();
    upstream.write_file("se/rd/serde", "serde\n").unwrap();

    // Capture upstream `master` state before the squash.
    let (original_head, original_tree) = {
        let repo = upstream.repository.lock().unwrap();
        let head = repo.find_reference("refs/heads/master").unwrap();
        let commit = head.peel_to_commit().unwrap();
        (commit.id(), commit.tree().unwrap().id())
    };

    let now = Utc::now().format("%F");

    assert_ok!(jobs::SquashIndex.enqueue(&conn).await);
    app.run_pending_background_jobs().await;

    let repo = upstream.repository.lock().unwrap();

    // `master` now points to a parentless commit with the squash message.
    let master = repo.find_reference("refs/heads/master").unwrap();
    let squashed = master.peel_to_commit().unwrap();
    assert_eq!(squashed.parent_count(), 0);
    assert!(
        squashed
            .message()
            .unwrap()
            .starts_with("Collapse index into one commit")
    );

    // Tree content is preserved — the squashed commit references the exact
    // same tree as the pre-squash HEAD.
    assert_eq!(squashed.tree().unwrap().id(), original_tree);

    // The archive branch captures the previous HEAD.
    let snapshot_ref = format!("refs/heads/snapshot-{now}");
    let snapshot = repo.find_reference(&snapshot_ref).unwrap();
    assert_eq!(snapshot.peel_to_commit().unwrap().id(), original_head);
}

/// Queue a one-shot `get_ref(refs/heads/master)` that returns `sha`.
fn expect_get_master(mock: &mut MockGitHubClient, sha: &'static str) {
    mock.expect_get_ref()
        .withf(|owner, repo, ref_name| owner == OWNER && repo == REPO && ref_name == MASTER_REF)
        .times(1)
        .returning(move |_, _, _| Ok(master_ref(sha)));
}

/// Queue a `get_commit(commit_sha)` that returns a commit pointing at `tree_sha`.
fn expect_get_commit(
    mock: &mut MockGitHubClient,
    commit_sha: &'static str,
    tree_sha: &'static str,
) {
    mock.expect_get_commit()
        .withf(move |owner, repo, sha| owner == OWNER && repo == REPO && sha == commit_sha)
        .times(1)
        .returning(move |_, _, _| {
            Ok(GitCommit {
                sha: commit_sha.into(),
                tree: GitObject {
                    sha: tree_sha.into(),
                },
            })
        });
}

/// Queue a parentless `create_commit(tree=tree_sha)` that returns `new_sha`.
fn expect_create_commit(
    mock: &mut MockGitHubClient,
    tree_sha: &'static str,
    new_sha: &'static str,
) {
    mock.expect_create_commit()
        .withf(move |owner, repo, input, _| {
            owner == OWNER
                && repo == REPO
                && input.tree == tree_sha
                && input.parents.is_empty()
                && input.message.starts_with("Collapse index into one commit")
        })
        .times(1)
        .returning(move |_, _, _, _| {
            Ok(GitCommit {
                sha: new_sha.into(),
                tree: GitObject {
                    sha: tree_sha.into(),
                },
            })
        });
}

/// Queue a `create_ref(refs/heads/snapshot-*, original_sha)`.
fn expect_create_snapshot_ref(mock: &mut MockGitHubClient, original_sha: &'static str) {
    mock.expect_create_ref()
        .withf(move |owner, repo, ref_name, sha, _| {
            owner == OWNER
                && repo == REPO
                && ref_name.starts_with("refs/heads/snapshot-")
                && sha == original_sha
        })
        .times(1)
        .returning(|_, _, ref_name, sha, _| {
            Ok(GitRef {
                ref_name: ref_name.to_string(),
                object: GitObject {
                    sha: sha.to_string(),
                },
            })
        });
}

/// Queue a forced `update_ref(refs/heads/master, new_sha, force=true)`.
fn expect_update_master(mock: &mut MockGitHubClient, new_sha: &'static str) {
    mock.expect_update_ref()
        .withf(move |owner, repo, ref_name, sha, force, _| {
            owner == OWNER && repo == REPO && ref_name == MASTER_REF && sha == new_sha && *force
        })
        .times(1)
        .returning(|_, _, ref_name, sha, _, _| {
            Ok(GitRef {
                ref_name: ref_name.to_string(),
                object: GitObject {
                    sha: sha.to_string(),
                },
            })
        });
}

/// `SquashIndexViaApi` should drive the squash entirely via the GitHub REST
/// API: read master, read its tree, create a parentless commit on the same
/// tree, create the snapshot ref, re-read master to guard against drift, and
/// fast-forward master to the new commit.
#[tokio::test(flavor = "multi_thread")]
async fn squash_index_via_api() {
    let mut github = MockGitHubClient::new();
    expect_get_master(&mut github, ORIGINAL_SHA);
    expect_get_commit(&mut github, ORIGINAL_SHA, TREE_SHA);
    expect_create_commit(&mut github, TREE_SHA, NEW_SHA);
    expect_create_snapshot_ref(&mut github, ORIGINAL_SHA);
    expect_get_master(&mut github, ORIGINAL_SHA); // drift check — still the same
    expect_update_master(&mut github, NEW_SHA);

    let (app, _) = TestApp::init()
        .with_github(github)
        .with_index_location(index_url())
        .with_job_runner()
        .empty()
        .await;

    let conn = app.db_conn().await;
    assert_ok!(jobs::SquashIndexViaApi.enqueue(&conn).await);
    app.run_pending_background_jobs().await;
}

/// If `master` has moved between the initial read and the drift check, the
/// job should bail without calling `update_ref`, leaving `master` unchanged
/// on the remote. The snapshot ref created earlier remains as a harmless
/// pointer to the pre-squash HEAD.
#[tokio::test(flavor = "multi_thread")]
async fn squash_index_via_api_bails_on_master_drift() {
    const DRIFTED_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";

    let mut github = MockGitHubClient::new();
    expect_get_master(&mut github, ORIGINAL_SHA);
    expect_get_commit(&mut github, ORIGINAL_SHA, TREE_SHA);
    expect_create_commit(&mut github, TREE_SHA, NEW_SHA);
    expect_create_snapshot_ref(&mut github, ORIGINAL_SHA);
    expect_get_master(&mut github, DRIFTED_SHA); // drift check — master has moved

    // `update_ref` is intentionally not queued; mockall panics on an
    // unexpected call, which is the assertion we want.

    let (app, _) = TestApp::init()
        .with_github(github)
        .with_index_location(index_url())
        .with_job_runner()
        .empty()
        .await;

    let mut conn = app.db_conn().await;
    assert_ok!(jobs::SquashIndexViaApi.enqueue(&conn).await);
    let err = app.try_run_pending_background_jobs().await.unwrap_err();
    assert_eq!(err.to_string(), "1 jobs failed");

    // Drain the failed job so the `TestAppInner::drop` empty-queue
    // post-condition is satisfied.
    diesel::delete(background_jobs::table)
        .execute(&mut conn)
        .await
        .unwrap();
}

use crate::util::TestApp;
use chrono::Utc;
use claims::assert_ok;
use crates_io::worker::jobs;
use crates_io_worker::BackgroundJob;

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

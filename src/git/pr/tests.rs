use super::*;
use crate::git::tests::Fixture;

fn resolve(root: &Path, reference: &str) -> String {
    let mut git = command(root);
    git.args(["rev-parse", "--verify", reference]);
    line(checked(git).unwrap(), "Commit").unwrap()
}

fn refs(root: &Path) -> Vec<u8> {
    let mut git = command(root);
    git.args(["show-ref"]);
    checked(git).unwrap()
}

fn context_file(changed: bool) -> String {
    (1..=30)
        .map(|number| {
            if changed && number == 15 {
                "pushed middle line\n".to_owned()
            } else {
                format!("context line {number}\n")
            }
        })
        .collect()
}

fn diverged_pull_request() -> (Fixture, Metadata, String) {
    let fixture = Fixture::new();
    fixture.write("context.txt", context_file(false).as_bytes());
    fixture.write("unchanged.txt", b"do not download this unchanged blob\n");
    fixture.commit("review context");
    fixture.git(&["tag", "-f", "baseline"]);
    let ancestor = resolve(fixture.root(), "baseline");
    fixture.git(&["checkout", "-b", "topic"]);
    fixture.write("file.txt", b"pushed topic\n");
    fixture.write("context.txt", context_file(true).as_bytes());
    fixture.write("topic.txt", b"topic addition\n");
    fixture.write(":(glob)*", b"literal PR addition\n");
    fs::remove_file(fixture.root().join("deleted.txt")).unwrap();
    fixture.commit("PR head");
    let head_ref_oid = resolve(fixture.root(), "HEAD");
    fixture.git(&["update-ref", "refs/pull/7/head", &head_ref_oid]);
    fixture.git(&["checkout", "main"]);
    fixture.write("file.txt", b"independent base change\n");
    fixture.write("base-only.txt", b"not part of the PR\n");
    fixture.commit("advance base independently");
    fixture.git(&["config", "uploadpack.allowFilter", "true"]);
    fixture.git(&["config", "uploadpack.allowAnySHA1InWant", "true"]);
    let base_ref_oid = resolve(fixture.root(), "HEAD");
    let metadata = Metadata {
        number: 7,
        url: "https://github.com/example/base/pull/7".into(),
        title: "A pushed PR".into(),
        base_ref_name: "main".into(),
        base_ref_oid,
        head_ref_oid,
    };
    (fixture, metadata, ancestor)
}

#[test]
fn borrowed_snapshots_preserve_target_worktree_index_and_refs_without_fetching() {
    let (fixture, metadata, ancestor) = diverged_pull_request();
    fixture.write("file.txt", b"local staged state\n");
    fixture.git(&["add", "file.txt"]);
    fixture.write("file.txt", b"local unstaged state\n");
    fixture.write("local-untracked.txt", b"not pushed\n");
    let before_index = fs::read(fixture.root().join(".git/index")).unwrap();
    let before_refs = refs(fixture.root());
    // Both snapshots are available through alternates: an unavailable remote
    // must not prevent reviewing them or trigger a needless network fetch.
    let remote = fixture
        .root()
        .join("unavailable-remote")
        .to_str()
        .unwrap()
        .to_owned();
    let pr =
        PullRequest::fetch(fixture.root(), metadata.clone(), remote, ancestor.clone()).unwrap();
    let review = pr.review(fixture.root().to_str().unwrap().into()).unwrap();
    assert_eq!(review.source, "pr");
    assert_eq!(review.base_oid, ancestor);
    assert_ne!(review.base_oid, metadata.base_ref_oid);
    assert_eq!(
        review.head_oid.as_deref(),
        Some(metadata.head_ref_oid.as_str())
    );
    assert_eq!(review.pr_url.as_deref(), Some(metadata.url.as_str()));
    assert_eq!(review.title.as_deref(), Some("A pushed PR"));
    let files: BTreeMap<_, _> = review
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    assert_eq!(files.len(), 5);
    assert_eq!(files["file.txt"].status, "M");
    assert!(files["file.txt"].patch.contains("-base\n"));
    assert!(files["file.txt"].patch.contains("+pushed topic\n"));
    assert!(!files["file.txt"].patch.contains("independent base change"));
    assert!(files["topic.txt"].patch.contains("+topic addition\n"));
    assert!(files[":(glob)*"].patch.contains("+literal PR addition\n"));
    assert_eq!(files["deleted.txt"].status, "D");
    assert!(files["deleted.txt"].patch.contains("-delete me\n"));
    for (path, old, new) in [
        ("file.txt", 1, 1),
        ("context.txt", 30, 30),
        ("topic.txt", 0, 1),
        (":(glob)*", 0, 1),
        ("deleted.txt", 1, 0),
    ] {
        assert_eq!(
            (files[path].old_lines, files[path].new_lines),
            (Some(old), Some(new)),
            "{path}"
        );
    }
    for file in &review.files {
        assert_eq!(file.patch, pr.patch(&file.path, 3).unwrap());
        assert!(!pr.patch(&file.path, 1_000_000).unwrap().is_empty());
    }
    assert_eq!(
        before_index,
        fs::read(fixture.root().join(".git/index")).unwrap()
    );
    assert_eq!(before_refs, refs(fixture.root()));
    assert_eq!(
        fs::read(fixture.root().join("file.txt")).unwrap(),
        b"local unstaged state\n"
    );
    assert_eq!(
        fs::read(fixture.root().join("local-untracked.txt")).unwrap(),
        b"not pushed\n"
    );
}

fn shallow_target(fixture: &Fixture, filter: bool) -> tempfile::TempDir {
    let target = tempfile::tempdir().unwrap();
    let remote = format!("file://{}", fixture.root().display());
    let mut clone = command(target.path());
    clone.args(["clone", "--depth=1", "--no-local", "--no-checkout"]);
    if filter {
        clone.arg("--filter=blob:none");
    }
    clone.args(["--", &remote, "."]);
    checked(clone).unwrap();
    target
}

fn has_object(root: &Path, oid: &str) -> bool {
    let mut probe = command(root);
    probe.env("GIT_NO_LAZY_FETCH", "1");
    probe.args(["cat-file", "-e", oid]);
    probe.output().unwrap().status.success()
}

#[test]
fn shallow_target_fetches_only_exact_snapshots_and_lazily_loads_changed_blobs() {
    let (fixture, metadata, ancestor) = diverged_pull_request();
    let target = shallow_target(&fixture, false);
    let remote = format!("file://{}", fixture.root().display());
    assert!(!has_object(target.path(), &ancestor));
    assert!(!has_object(target.path(), &metadata.head_ref_oid));
    let before_refs = refs(target.path());
    let before_shallow = fs::read(target.path().join(".git/shallow")).unwrap();
    let older_ancestor = resolve(fixture.root(), "baseline^");
    let old_blob = resolve(fixture.root(), "baseline:context.txt");
    let new_blob = resolve(fixture.root(), "topic:context.txt");
    let pr = PullRequest::fetch(target.path(), metadata, remote, ancestor.clone()).unwrap();
    assert_eq!(pr.merge_base, ancestor);
    assert!(!has_object(pr.directory.path(), &older_ancestor));
    assert!(!has_object(pr.directory.path(), &new_blob));
    let mut shallow = command(pr.directory.path());
    shallow.args(["rev-parse", "--is-shallow-repository"]);
    assert_eq!(
        line(checked(shallow).unwrap(), "Shallow status").unwrap(),
        "true"
    );
    let patch = pr.patch("context.txt", 3).unwrap();
    assert!(patch.contains("-context line 15\n"));
    assert!(patch.contains("+pushed middle line\n"));
    assert!(!patch.contains("context line 1\n"));
    assert!(has_object(pr.directory.path(), &old_blob));
    assert!(has_object(pr.directory.path(), &new_blob));
    assert!(!has_object(target.path(), &new_blob));
    let full = pr.patch("context.txt", 1_000_000).unwrap();
    assert!(full.contains(" context line 1\n"));
    assert!(full.contains(" context line 30\n"));
    let review = pr.review(target.path().to_str().unwrap().into()).unwrap();
    assert!(review
        .files
        .iter()
        .any(|file| file.path == "file.txt" && file.patch.contains("+pushed topic\n")));
    let context = review
        .files
        .iter()
        .find(|file| file.path == "context.txt")
        .unwrap();
    assert_eq!((context.old_lines, context.new_lines), (Some(30), Some(30)));
    assert!(!has_object(pr.directory.path(), &older_ancestor));
    assert_eq!(before_refs, refs(target.path()));
    assert_eq!(
        before_shallow,
        fs::read(target.path().join(".git/shallow")).unwrap()
    );
    assert!(!target.path().join(".git/index").exists());
}

#[test]
fn bare_partial_review_fetches_missing_blobs_without_filling_target_object_store() {
    let (fixture, metadata, ancestor) = diverged_pull_request();
    let target = shallow_target(&fixture, true);
    let unchanged_blob = resolve(fixture.root(), "main:unchanged.txt");
    assert!(!has_object(target.path(), &unchanged_blob));
    let pr = PullRequest::fetch(
        target.path(),
        metadata,
        format!("file://{}", fixture.root().display()),
        ancestor,
    )
    .unwrap();
    assert!(!has_object(pr.directory.path(), &unchanged_blob));
    let head_blob = resolve(fixture.root(), "topic:context.txt");
    assert!(!has_object(pr.directory.path(), &head_blob));
    assert_eq!(pr.files().unwrap().len(), 5);
    // File enumeration batches the missing changed blobs before patch rendering.
    assert!(has_object(pr.directory.path(), &head_blob));
    assert!(!has_object(pr.directory.path(), &unchanged_blob));
    let review = pr.review(target.path().to_str().unwrap().into()).unwrap();
    assert_eq!(review.files.len(), 5);
    let context = review
        .files
        .iter()
        .find(|file| file.path == "context.txt")
        .unwrap();
    assert_eq!((context.old_lines, context.new_lines), (Some(30), Some(30)));
    assert!(pr
        .patch("context.txt", 1_000_000)
        .unwrap()
        .contains(" context line 30\n"));
    assert!(!has_object(pr.directory.path(), &unchanged_blob));
    assert!(!has_object(target.path(), &unchanged_blob));
    assert!(has_object(pr.directory.path(), &head_blob));
    assert!(!has_object(target.path(), &head_blob));
}

#[test]
fn metadata_head_is_pinned_even_after_pull_ref_moves() {
    let (fixture, metadata, ancestor) = diverged_pull_request();
    fixture.git(&["update-ref", "refs/pull/7/head", &metadata.base_ref_oid]);
    let target = shallow_target(&fixture, false);
    let pr = PullRequest::fetch(
        target.path(),
        metadata.clone(),
        format!("file://{}", fixture.root().display()),
        ancestor,
    )
    .unwrap();
    assert_eq!(
        resolve(pr.directory.path(), "refs/review/head"),
        metadata.head_ref_oid
    );
    assert!(pr.patch("file.txt", 3).unwrap().contains("+pushed topic\n"));
}

#[test]
fn historical_merge_base_is_fetched_instead_of_current_base_branch_tip() {
    let (fixture, mut metadata, ancestor) = diverged_pull_request();
    metadata.base_ref_oid = ancestor.clone();
    assert_ne!(resolve(fixture.root(), "main"), metadata.base_ref_oid);
    let target = shallow_target(&fixture, false);
    let pr = PullRequest::fetch(
        target.path(),
        metadata.clone(),
        format!("file://{}", fixture.root().display()),
        ancestor.clone(),
    )
    .unwrap();
    assert_eq!(resolve(pr.directory.path(), "refs/review/base"), ancestor);
    assert_eq!(
        pr.review(target.path().to_str().unwrap().into())
            .unwrap()
            .head_oid,
        Some(metadata.head_ref_oid)
    );
}

#[test]
fn linked_worktree_reuses_common_objects_without_fetching() {
    let (fixture, metadata, ancestor) = diverged_pull_request();
    let worktree = tempfile::tempdir().unwrap();
    let path = worktree.path().join("linked");
    fixture.git(&[
        "worktree",
        "add",
        "--detach",
        path.to_str().unwrap(),
        "main",
    ]);
    let before_refs = refs(&path);
    let index = fixture.root().join(".git/worktrees/linked/index");
    let before_index = fs::read(&index).unwrap();
    let pr = PullRequest::fetch(
        &path,
        metadata,
        fixture
            .root()
            .join("unavailable-remote")
            .to_str()
            .unwrap()
            .into(),
        ancestor,
    )
    .unwrap();
    assert!(pr.patch("file.txt", 3).unwrap().contains("+pushed topic\n"));
    assert_eq!(before_refs, refs(&path));
    assert_eq!(before_index, fs::read(index).unwrap());
}

#[test]
fn validates_github_metadata_before_using_remote_or_compare_endpoint() {
    let (_, mut metadata, _) = diverged_pull_request();
    assert_eq!(
        metadata.remote().unwrap(),
        "https://github.com/example/base.git"
    );
    for invalid in [
        "http://github.com/example/base/pull/7",
        "https://user@github.com/example/base/pull/7",
        "https://github.com/example/base/pull/8",
        "https://github.com/example/base/pull/7?redirect=other",
        "https://github.com/example/../pull/7",
        "https://github.com/example/base/extra/pull/7",
    ] {
        metadata.url = invalid.into();
        assert!(metadata.remote().is_err(), "accepted {invalid}");
    }
    metadata.url = "https://github.example.com:8443/example/base/pull/7".into();
    assert_eq!(
        metadata.remote().unwrap(),
        "https://github.example.com:8443/example/base.git"
    );
}

#[test]
fn gitlinks_are_reviewed_without_fetching_submodule_commits() {
    let (fixture, mut metadata, ancestor) = diverged_pull_request();
    let submodule_commit = "1234567890123456789012345678901234567890";
    fixture.git(&["checkout", "topic"]);
    fixture.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        "160000",
        submodule_commit,
        "module",
    ]);
    fixture.git(&["commit", "-m", "add submodule link"]);
    metadata.head_ref_oid = resolve(fixture.root(), "HEAD");
    let pr = PullRequest::fetch(
        fixture.root(),
        metadata,
        fixture
            .root()
            .join("unavailable-remote")
            .to_str()
            .unwrap()
            .into(),
        ancestor,
    )
    .unwrap();
    let review = pr.review(fixture.root().to_str().unwrap().into()).unwrap();
    let module = review
        .files
        .iter()
        .find(|file| file.path == "module")
        .unwrap();
    assert_eq!(module.status, "A");
    assert_eq!((module.old_lines, module.new_lines), (None, None));
    assert!(module
        .patch
        .contains(&format!("+Subproject commit {submodule_commit}")));
    assert!(!has_object(pr.directory.path(), submodule_commit));
}

#[test]
fn shallow_snapshot_totals_handle_empty_unterminated_and_binary_contents() {
    let fixture = Fixture::new();
    fixture.write("deleted.txt", b"first\nlast");
    fixture.write("empty-deleted.txt", b"");
    fixture.write("file.txt", b"old\nmiddle\nlast");
    fixture.commit("snapshot base");
    let base = resolve(fixture.root(), "HEAD");
    fixture.git(&["checkout", "-b", "topic"]);
    fixture.write("file.txt", b"new\nlast");
    fixture.write("new.txt", b"first\nlast");
    fixture.write("empty-new.txt", b"");
    fixture.write("binary.bin", b"\0binary\nnot text\n");
    fs::remove_file(fixture.root().join("deleted.txt")).unwrap();
    fs::remove_file(fixture.root().join("empty-deleted.txt")).unwrap();
    fixture.commit("snapshot head");
    let head = resolve(fixture.root(), "HEAD");
    fixture.git(&["config", "uploadpack.allowFilter", "true"]);
    fixture.git(&["config", "uploadpack.allowAnySHA1InWant", "true"]);
    let target = shallow_target(&fixture, true);
    let metadata = Metadata {
        number: 7,
        url: "https://github.com/example/base/pull/7".into(),
        title: "Line totals".into(),
        base_ref_name: "main".into(),
        base_ref_oid: base.clone(),
        head_ref_oid: head,
    };
    let pr = PullRequest::fetch(
        target.path(),
        metadata,
        format!("file://{}", fixture.root().display()),
        base,
    )
    .unwrap();
    let review = pr.review(target.path().to_str().unwrap().into()).unwrap();
    let files: BTreeMap<_, _> = review
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    for (path, old, new) in [
        ("file.txt", 3, 2),
        ("new.txt", 0, 2),
        ("empty-new.txt", 0, 0),
        ("deleted.txt", 2, 0),
        ("empty-deleted.txt", 0, 0),
    ] {
        assert_eq!(
            (files[path].old_lines, files[path].new_lines),
            (Some(old), Some(new)),
            "{path}"
        );
    }
    assert_eq!(
        (files["binary.bin"].old_lines, files["binary.bin"].new_lines),
        (None, None)
    );
    assert!(!target.path().join(".git/index").exists());
}

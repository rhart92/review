use super::*;
use std::collections::BTreeMap;

pub(super) struct Fixture {
    directory: tempfile::TempDir,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let fixture = Self {
            directory: tempfile::tempdir().unwrap(),
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "Review Test"]);
        fixture.git(&["config", "user.email", "review@example.invalid"]);
        fixture.write("file.txt", b"base\n");
        fixture.write("deleted.txt", b"delete me\n");
        fixture.write(".gitignore", b"ignored*\n");
        fixture.commit("base");
        fixture.git(&["tag", "baseline"]);
        fixture
    }

    pub(super) fn root(&self) -> &Path {
        self.directory.path()
    }

    pub(super) fn git(&self, args: &[&str]) {
        let mut git = command(self.root());
        git.args(args);
        checked(git).unwrap();
    }

    pub(super) fn write(&self, path: &str, contents: &[u8]) {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    pub(super) fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", message]);
    }

    fn repository(&self) -> Repository {
        Repository::open(self.root(), "baseline".into()).unwrap()
    }

    fn files(&self) -> BTreeMap<String, String> {
        self.repository()
            .review()
            .unwrap()
            .files
            .into_iter()
            .map(|file| (file.path, file.status))
            .collect()
    }
}

#[test]
fn combines_committed_staged_unstaged_and_untracked_without_mutating_index() {
    let fixture = Fixture::new();
    fixture.write("file.txt", b"committed\n");
    fixture.commit("after base");
    fixture.write("file.txt", b"staged\n");
    fixture.git(&["add", "file.txt"]);
    fixture.write("file.txt", b"live\n");
    fixture.write("new.txt", b"new\n");
    fixture.write("ignored-file", b"not shown\n");
    let original_index = fs::read(fixture.root().join(".git/index")).unwrap();
    let files = fixture.files();
    assert_eq!(files.len(), 2);
    assert_eq!(files["file.txt"], "M");
    assert_eq!(files["new.txt"], "A");
    let patch = fixture.repository().diff("file.txt", 3).unwrap();
    assert!(patch.contains("-base\n"));
    assert!(patch.contains("+live\n"));
    assert!(!patch.contains("+staged\n"));
    assert_eq!(
        original_index,
        fs::read(fixture.root().join(".git/index")).unwrap()
    );
}

#[test]
fn private_index_does_not_inherit_hidden_worktree_edits() {
    let fixture = Fixture::new();
    fixture.git(&["update-index", "--assume-unchanged", "file.txt"]);
    fixture.git(&["update-index", "--skip-worktree", "deleted.txt"]);
    fixture.write("file.txt", b"visible assumed edit\n");
    fixture.write("deleted.txt", b"visible skipped edit\n");
    let original_index = fs::read(fixture.root().join(".git/index")).unwrap();
    let mut repository = fixture.repository();
    for _ in 0..2 {
        let review = repository.review().unwrap();
        assert_eq!(review.files.len(), 2);
        assert!(review
            .files
            .iter()
            .any(|file| file.patch.contains("+visible assumed edit")));
        assert!(review
            .files
            .iter()
            .any(|file| file.patch.contains("+visible skipped edit")));
    }
    assert_eq!(
        original_index,
        fs::read(fixture.root().join(".git/index")).unwrap()
    );
}

#[test]
fn canceled_staged_edits_and_restored_staged_deletions_disappear() {
    let fixture = Fixture::new();
    fixture.write("file.txt", b"staged\n");
    fixture.git(&["add", "file.txt"]);
    fixture.write("file.txt", b"base\n");
    fixture.git(&["rm", "deleted.txt"]);
    fixture.write("deleted.txt", b"delete me\n");
    assert!(fixture.files().is_empty());
    fixture.write("deleted.txt", b"restored with edits\n");
    let review = fixture.repository().review().unwrap();
    let restored = review
        .files
        .iter()
        .find(|file| file.path == "deleted.txt")
        .unwrap();
    assert_eq!((restored.old_lines, restored.new_lines), (Some(1), Some(1)));
    assert_eq!(fixture.files()["deleted.txt"], "M");
    assert!(fixture
        .repository()
        .diff("deleted.txt", 0)
        .unwrap()
        .contains("+restored with edits"));
}

#[test]
fn handles_deletions_binary_and_literal_unicode_filenames() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root().join("deleted.txt")).unwrap();
    fixture.write("binary.bin", b"\0binary\xff\n");
    let unusual = "spaces café\tline\n[1].txt";
    fixture.write(unusual, b"hello\n");
    fixture.write(":(glob)*", b"literal\n");
    let files = fixture.files();
    assert_eq!(files["deleted.txt"], "D");
    assert_eq!(files[unusual], "A");
    assert_eq!(files[":(glob)*"], "A");
    assert!(fixture
        .repository()
        .diff("deleted.txt", 3)
        .unwrap()
        .contains("-delete me"));
    assert!(fixture
        .repository()
        .diff("binary.bin", 3)
        .unwrap()
        .contains("Binary files"));
    assert!(fixture
        .repository()
        .diff(unusual, 1_000_000)
        .unwrap()
        .contains("+hello"));
    assert!(fixture
        .repository()
        .diff(":(glob)*", 3)
        .unwrap()
        .contains("+literal"));
}

#[test]
fn shows_forced_ignored_additions_but_not_additions_deleted_from_worktree() {
    let fixture = Fixture::new();
    fixture.write("ignored-forced", b"included\n");
    fixture.git(&["add", "-f", "ignored-forced"]);
    fixture.write("added-then-removed", b"gone\n");
    fixture.git(&["add", "added-then-removed"]);
    fs::remove_file(fixture.root().join("added-then-removed")).unwrap();
    assert_eq!(
        fixture.files().keys().cloned().collect::<Vec<_>>(),
        vec!["ignored-forced"]
    );
    assert!(fixture
        .repository()
        .diff("ignored-forced", 3)
        .unwrap()
        .contains("+included"));
}

#[test]
fn rejects_unsafe_unchanged_and_disappeared_paths() {
    let fixture = Fixture::new();
    for path in [
        "../outside",
        "/etc/passwd",
        ".git/config",
        "",
        "file.txt",
        "missing",
        "bad\0path",
    ] {
        assert!(
            fixture.repository().diff(path, 3).is_err(),
            "accepted {path:?}"
        );
    }
    fixture.write("transient", b"here\n");
    assert!(fixture.files().contains_key("transient"));
    fs::remove_file(fixture.root().join("transient")).unwrap();
    let error = fixture.repository().diff("transient", 3).unwrap_err();
    assert!(error.contains("Refresh"));
    assert!(fixture.repository().diff("file.txt", 1_000_001).is_err());
}

#[cfg(unix)]
#[test]
fn diffs_symlink_itself_and_rejects_symlink_ancestors() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), b"SECRET CONTENT\n").unwrap();
    symlink(outside.path().join("secret"), fixture.root().join("link")).unwrap();
    let patch = fixture.repository().diff("link", 3).unwrap();
    assert!(!patch.contains("SECRET CONTENT"));
    assert!(patch.contains("120000"));
    let review = fixture.repository().review().unwrap();
    let link = review
        .files
        .iter()
        .find(|file| file.path == "link")
        .unwrap();
    assert_eq!((link.old_lines, link.new_lines), (Some(0), Some(1)));
    // The target has several lines, but the stored symlink is still one line.
    fs::write(
        outside.path().join("secret"),
        b"SECRET\nCONTENT\nMUST NOT LEAK\n",
    )
    .unwrap();
    let review = fixture.repository().review().unwrap();
    let link = review
        .files
        .iter()
        .find(|file| file.path == "link")
        .unwrap();
    assert_eq!(link.new_lines, Some(1));
    assert!(!link.patch.contains("MUST NOT LEAK"));
    fixture.write("nested/secret", b"original\n");
    fixture.commit("nested file");
    // Compare against a base that actually contains the nested tracked file.
    fixture.git(&["tag", "-f", "baseline"]);
    fs::remove_dir_all(fixture.root().join("nested")).unwrap();
    symlink(outside.path(), fixture.root().join("nested")).unwrap();
    assert!(fixture.repository().diff("nested/secret", 3).is_err());
}

#[test]
fn resolves_live_base_and_rejects_invalid_repository_or_ref() {
    let fixture = Fixture::new();
    assert!(Repository::open(fixture.root(), "does-not-exist".into()).is_err());
    let outside = tempfile::tempdir().unwrap();
    assert!(Repository::open(outside.path(), "main".into()).is_err());
    let mut repository = Repository::open(fixture.root(), "main".into()).unwrap();
    let before = repository.review().unwrap().base_oid;
    fixture.write("file.txt", b"next commit\n");
    fixture.commit("advance main");
    let after = repository.review().unwrap();
    assert_ne!(before, after.base_oid);
    assert!(after.files.is_empty());
}

#[test]
fn whole_review_contains_every_net_patch_and_preserves_index_and_refs() {
    let fixture = Fixture::new();
    fixture.write("committed.txt", b"committed addition\n");
    fixture.commit("committed change");
    fixture.write("file.txt", b"staged change\n");
    fixture.git(&["add", "file.txt"]);
    fixture.write("untracked.txt", b"untracked addition\n");
    fixture.write(":(glob)*", b"literal addition\n");
    fs::remove_file(fixture.root().join("deleted.txt")).unwrap();
    let index = fs::read(fixture.root().join(".git/index")).unwrap();
    let mut refs = command(fixture.root());
    refs.args(["show-ref"]);
    let refs = checked(refs).unwrap();
    let mut repository = fixture.repository();
    let review = repository.review().unwrap();
    assert_eq!(review.source, "local");
    assert!(review.head_oid.is_none());
    assert!(review.pr_url.is_none());
    let files: BTreeMap<_, _> = review
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    assert_eq!(files.len(), 5);
    for (path, status, expected) in [
        ("committed.txt", "A", "+committed addition\n"),
        ("file.txt", "M", "+staged change\n"),
        ("untracked.txt", "A", "+untracked addition\n"),
        ("deleted.txt", "D", "-delete me\n"),
        (":(glob)*", "A", "+literal addition\n"),
    ] {
        assert_eq!(files[path].status, status);
        assert!(
            files[path].patch.contains(expected),
            "missing patch for {path}"
        );
        assert_eq!(files[path].patch, repository.diff(path, 3).unwrap());
    }
    assert_eq!(index, fs::read(fixture.root().join(".git/index")).unwrap());
    let mut after = command(fixture.root());
    after.args(["show-ref"]);
    assert_eq!(refs, checked(after).unwrap());
}

#[test]
fn explicit_local_query_overrides_default_pr_without_changing_it() {
    let fixture = Fixture::new();
    let mut repository =
        Repository::open_with_pr(fixture.root(), "baseline".into(), Some(7)).unwrap();
    fixture.write("file.txt", b"local\n");
    assert_eq!(repository.review_source(Some(0)).unwrap().source, "local");
    assert!(repository
        .diff_source("file.txt", 3, Some(0))
        .unwrap()
        .contains("+local\n"));
    assert_eq!(repository.default_pr, Some(7));
    // PR-only startup must not require the local configured base to exist.
    assert!(Repository::open_with_pr(fixture.root(), "missing".into(), Some(7)).is_ok());
}

#[test]
fn exact_totals_distinguish_three_trailing_context_lines_from_hidden_content() {
    let fixture = Fixture::new();
    fixture.write("ends-at-context.txt", b"old\none\ntwo\nthree\n");
    fixture.write("hidden-tail.txt", b"old\none\ntwo\nthree\nfour\n");
    fixture.commit("context baselines");
    fixture.git(&["tag", "-f", "baseline"]);
    fixture.write("ends-at-context.txt", b"new\none\ntwo\nthree\n");
    fixture.write("hidden-tail.txt", b"new\none\ntwo\nthree\nfour\n");
    let review = fixture.repository().review().unwrap();
    let files: BTreeMap<_, _> = review
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let complete = files["ends-at-context.txt"];
    let hidden = files["hidden-tail.txt"];
    assert_eq!((complete.old_lines, complete.new_lines), (Some(4), Some(4)));
    assert_eq!((hidden.old_lines, hidden.new_lines), (Some(5), Some(5)));
    assert!(complete.patch.contains("@@ -1,4 +1,4 @@"));
    assert!(hidden.patch.contains("@@ -1,4 +1,4 @@"));
    assert!(!hidden.patch.contains("four"));
}

#[test]
fn counts_net_text_lines_without_final_newlines_and_keeps_binary_totals_null() {
    let fixture = Fixture::new();
    fixture.write("deleted-no-newline.txt", b"first\nlast");
    fixture.write("deleted-empty.txt", b"");
    fixture.write("file.txt", b"old\nsecond\nthird");
    fixture.commit("line-count baselines");
    fixture.git(&["tag", "-f", "baseline"]);
    fixture.write("file.txt", b"staged\nstate\nis\nnot\nthe\nworktree\n");
    fixture.git(&["add", "file.txt"]);
    fixture.write("file.txt", b"live\nlast");
    fixture.write("spaces café\tline\n[1].txt", b"first\nlast");
    fixture.write("empty.txt", b"");
    fixture.write("binary.bin", b"\0not\ntext\n");
    fs::remove_file(fixture.root().join("deleted-no-newline.txt")).unwrap();
    fs::remove_file(fixture.root().join("deleted-empty.txt")).unwrap();
    let review = fixture.repository().review().unwrap();
    let files: BTreeMap<_, _> = review
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    for (path, old, new) in [
        ("file.txt", 3, 2),
        ("spaces café\tline\n[1].txt", 0, 2),
        ("empty.txt", 0, 0),
        ("deleted-no-newline.txt", 2, 0),
        ("deleted-empty.txt", 0, 0),
    ] {
        assert_eq!(
            (files[path].old_lines, files[path].new_lines),
            (Some(old), Some(new)),
            "{path:?}"
        );
    }
    assert_eq!(
        (files["binary.bin"].old_lines, files["binary.bin"].new_lines),
        (None, None)
    );
    let json = serde_json::to_value(&review).unwrap();
    let binary = json["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "binary.bin")
        .unwrap();
    assert!(binary["old_lines"].is_null());
    assert!(binary["new_lines"].is_null());
}

#[cfg(unix)]
#[test]
fn modified_and_deleted_symlink_totals_count_link_text_not_target_files() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let old_target = outside.path().join("old");
    let new_target = outside.path().join("new\nname");
    fs::write(&old_target, b"SECRET\nOLD\nTARGET\nCONTENTS\n").unwrap();
    fs::write(&new_target, b"SECRET\nNEW\nTARGET\nCONTENTS\n").unwrap();
    symlink(&old_target, fixture.root().join("link")).unwrap();
    symlink(&old_target, fixture.root().join("deleted-link")).unwrap();
    fixture.commit("symlink baseline");
    fixture.git(&["tag", "-f", "baseline"]);
    fs::remove_file(fixture.root().join("link")).unwrap();
    fs::remove_file(fixture.root().join("deleted-link")).unwrap();
    symlink(&new_target, fixture.root().join("link")).unwrap();
    let review = fixture.repository().review().unwrap();
    let files: BTreeMap<_, _> = review
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    assert_eq!(
        (files["link"].old_lines, files["link"].new_lines),
        (Some(1), Some(2))
    );
    assert_eq!(
        (
            files["deleted-link"].old_lines,
            files["deleted-link"].new_lines
        ),
        (Some(1), Some(0))
    );
    for file in &review.files {
        assert!(!file.patch.contains("SECRET"));
        assert!(!file.patch.contains("CONTENTS"));
    }
}

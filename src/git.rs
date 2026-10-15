mod pr;

use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsStr,
    fs, io,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

pub type Result<T, E = String> = std::result::Result<T, E>;

pub struct Repository {
    root: PathBuf,
    base: String,
    index: Option<BaseIndex>,
    default_pr: Option<u64>,
    pull_requests: HashMap<u64, pr::PullRequest>,
}

struct BaseIndex {
    oid: String,
    _directory: tempfile::TempDir,
    path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    pub patch: String,
    pub old_lines: Option<usize>,
    pub new_lines: Option<usize>,
}

#[derive(Serialize)]
pub struct Review {
    pub repo: String,
    pub base: String,
    pub base_oid: String,
    pub branch: String,
    pub files: Vec<ChangedFile>,
    pub source: &'static str,
    pub head_oid: Option<String>,
    pub pr_url: Option<String>,
    pub title: Option<String>,
}

impl ChangedFile {
    // Count actual hunk rows, never filename headers or no-newline markers.
    // With the current file's total this also gives the exact base total,
    // without opening a second copy of the file.
    fn line_delta(&self) -> Option<(usize, usize)> {
        let mut added = 0;
        let mut removed = 0;
        let mut in_hunk = false;
        let mut textual = false;
        for row in self.patch.lines() {
            if row.starts_with("diff --git ") {
                in_hunk = false;
            } else if row.starts_with("@@ ") {
                in_hunk = true;
                textual = true;
            } else if in_hunk {
                match row.as_bytes().first() {
                    Some(b'+') => added += 1,
                    Some(b'-') => removed += 1,
                    _ => {}
                }
            } else if row.starts_with("Binary files ")
                || row == "GIT binary patch"
                || row.split(' ').any(|field| field == "160000")
            {
                return None;
            } else if row.starts_with("new file mode ") || row.starts_with("deleted file mode ") {
                // Empty textual additions/deletions have no hunk.
                textual = true;
            }
        }
        textual.then_some((added, removed))
    }

    fn set_line_totals(&mut self, new: usize, delta: (usize, usize)) -> Result<()> {
        let old = new
            .checked_add(delta.1)
            .and_then(|total| total.checked_sub(delta.0))
            .ok_or_else(|| {
                format!(
                    "File {:?} changed while reading; refresh the file list",
                    self.path
                )
            })?;
        self.old_lines = Some(old);
        self.new_lines = Some(new);
        Ok(())
    }
}

fn count_lines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|byte| **byte == b'\n').count()
        + usize::from(bytes.last().is_some_and(|byte| *byte != b'\n'))
}

fn count_reader_lines(mut reader: impl io::BufRead) -> io::Result<usize> {
    let mut lines = 0;
    let mut last = None;
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            break;
        }
        lines += bytes.iter().filter(|byte| **byte == b'\n').count();
        last = bytes.last().copied();
        let length = bytes.len();
        reader.consume(length);
    }
    Ok(lines + usize::from(last.is_some_and(|byte| byte != b'\n')))
}

fn command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("--no-pager")
        .arg("--no-optional-locks")
        .arg("--literal-pathspecs")
        .args(["-c", "core.fsmonitor=false", "-c", "core.quotepath=false"])
        .args([
            "-c",
            "core.splitIndex=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "core.sparseCheckout=false",
            "-c",
            "index.sparse=false",
        ])
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "submodule.recurse=false",
        ])
        .arg("-C")
        .arg(root)
        // An inherited Git environment must not redirect reads to another repo/index.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_SHALLOW_FILE")
        .env_remove("GIT_NAMESPACE")
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn checked(mut command: Command) -> Result<Vec<u8>> {
    let output = command
        .output()
        .map_err(|error| format!("Cannot run Git: {error}"))?;
    if !output.status.success() {
        return Err(git_error(&output));
    }
    Ok(output.stdout)
}

fn git_error(output: &Output) -> String {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        format!("Git exited with {}", output.status)
    } else {
        format!("Git: {detail}")
    }
}

fn text(bytes: Vec<u8>, description: &str) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| format!("{description} is not valid UTF-8"))
}

fn line(bytes: Vec<u8>, description: &str) -> Result<String> {
    let mut value = text(bytes, description)?;
    if value.ends_with('\n') {
        value.pop();
    }
    Ok(value)
}

/// Discover a working-tree root without resolving a base or loading a diff.
/// A directory outside a working tree is a normal result for the folder picker.
pub fn discover_root(path: &Path) -> Result<Option<PathBuf>> {
    let mut discover = command(path);
    discover.args(["rev-parse", "--show-toplevel"]);
    discover.env("LC_ALL", "C");
    let output = discover
        .output()
        .map_err(|error| format!("Cannot run Git: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        if output.status.code() == Some(128)
            && (detail.contains("not a git repository")
                || detail.contains("must be run in a work tree"))
        {
            return Ok(None);
        }
        return Err(git_error(&output));
    }
    let root = PathBuf::from(line(output.stdout, "Repository root")?);
    let root =
        fs::canonicalize(root).map_err(|error| format!("Cannot open repository: {error}"))?;
    if root.to_str().is_none() {
        return Err("Repository root is not valid UTF-8".into());
    }
    Ok(Some(root))
}

impl Repository {
    #[cfg(test)]
    pub fn open(path: &Path, base: String) -> Result<Self> {
        Self::open_with_pr(path, base, None)
    }

    pub fn open_with_pr(path: &Path, base: String, default_pr: Option<u64>) -> Result<Self> {
        let root = discover_root(path)?
            .ok_or_else(|| format!("Not a Git working tree: {}", path.display()))?;
        let repository = Self::from_root(root, base, default_pr);
        if repository.default_pr.is_none() {
            repository.resolve_base()?;
        }
        Ok(repository)
    }

    /// The registry has already canonicalized this root. Base resolution is deferred
    /// until local review, so PRs and the picker do not require a local base ref.
    pub fn from_root(root: PathBuf, base: String, default_pr: Option<u64>) -> Self {
        Self {
            root,
            base,
            index: None,
            default_pr: default_pr.filter(|number| *number != 0),
            pull_requests: HashMap::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve_base(&self) -> Result<String> {
        let mut resolve = command(&self.root);
        resolve.args(["rev-parse", "--verify", "--end-of-options"]);
        resolve.arg(format!("{}^{{commit}}", self.base));
        line(
            checked(resolve)
                .map_err(|error| format!("Cannot resolve base {:?}: {error}", self.base))?,
            "Base commit",
        )
    }

    fn changed_files(&self, index: &Path) -> Result<Vec<ChangedFile>> {
        let mut diff = command(&self.root);
        diff.env("GIT_INDEX_FILE", index);
        diff.args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--name-status",
            "-z",
            "--",
        ]);
        let bytes = checked(diff)?;
        let mut fields = bytes.split(|byte| *byte == 0);
        let mut files = BTreeMap::new();
        while let Some(status) = fields.next() {
            if status.is_empty() {
                break;
            }
            let path = fields
                .next()
                .ok_or("Git returned an incomplete file status")?;
            let path = text(path.to_vec(), "Changed filename")?;
            validate_relative(&path)?;
            files.insert(path, text(status.to_vec(), "File status")?);
        }
        let mut untracked = command(&self.root);
        untracked.env("GIT_INDEX_FILE", index);
        untracked.args(["ls-files", "--others", "--exclude-standard", "-z", "--"]);
        for path in checked(untracked)?
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path = text(path.to_vec(), "Untracked filename")?;
            validate_relative(&path)?;
            files.entry(path).or_insert_with(|| "A".into());
        }
        // Explicitly staged, ignored additions must also remain visible.
        let mut base_files = command(&self.root);
        base_files.env("GIT_INDEX_FILE", index);
        base_files.args(["ls-files", "--cached", "-z", "--"]);
        let base_bytes = checked(base_files)?;
        let base_paths: HashSet<&[u8]> = base_bytes.split(|byte| *byte == 0).collect();
        let mut cached = command(&self.root);
        cached.args(["ls-files", "--cached", "-z", "--"]);
        for path in checked(cached)?
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path = text(path.to_vec(), "Indexed filename")?;
            validate_relative(&path)?;
            if !files.contains_key(&path)
                && !base_paths.contains(path.as_bytes())
                && fs::symlink_metadata(self.root.join(&path)).is_ok()
            {
                files.insert(path, "A".into());
            }
        }
        Ok(files
            .into_iter()
            .map(|(path, status)| ChangedFile {
                path,
                status,
                patch: String::new(),
                old_lines: None,
                new_lines: None,
            })
            .collect())
    }

    fn ensure_base_index(&mut self, oid: &str) -> Result<()> {
        if self.index.as_ref().is_some_and(|index| index.oid == oid) {
            return Ok(());
        }
        let directory = tempfile::tempdir()
            .map_err(|error| format!("Cannot create temporary index: {error}"))?;
        let path = directory.path().join("index");
        // Seed stat information from the real index without ever writing to it.
        // A fresh read-tree index has no stat cache and rehashes the entire checkout.
        let mut index_path = command(&self.root);
        index_path.args(["rev-parse", "--path-format=absolute", "--git-path", "index"]);
        let source = PathBuf::from(line(checked(index_path)?, "Index path")?);
        match fs::copy(&source, &path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot copy repository index: {error}")),
        }
        let mut read = command(&self.root);
        read.env("GIT_INDEX_FILE", &path);
        read.args(["read-tree", "--reset", "--no-sparse-checkout", oid]);
        checked(read)?;
        // Copied user flags must not hide edits from this independent review.
        let mut entries = command(&self.root);
        entries.env("GIT_INDEX_FILE", &path);
        entries.args(["ls-files", "--cached", "-z"]);
        let entries_path = directory.path().join("paths");
        fs::write(&entries_path, checked(entries)?)
            .map_err(|error| format!("Cannot write private index paths: {error}"))?;
        for flag in ["--no-assume-unchanged", "--no-skip-worktree"] {
            let mut clear = command(&self.root);
            clear.env("GIT_INDEX_FILE", &path);
            clear.args(["update-index", flag, "-z", "--stdin"]);
            clear.stdin(
                fs::File::open(&entries_path)
                    .map_err(|error| format!("Cannot read private index paths: {error}"))?,
            );
            checked(clear)?;
        }
        // Persist refreshed stats only in our private index. Exit 1 means some
        // entries differ from the worktree, which is expected for a review.
        let mut refresh = command(&self.root);
        refresh.env("GIT_INDEX_FILE", &path);
        refresh.args(["update-index", "--really-refresh", "--ignore-submodules"]);
        let output = refresh
            .output()
            .map_err(|error| format!("Cannot refresh private index: {error}"))?;
        if !output.status.success() && output.status.code() != Some(1) {
            return Err(git_error(&output));
        }
        self.index = Some(BaseIndex {
            oid: oid.into(),
            _directory: directory,
            path,
        });
        Ok(())
    }

    #[cfg(test)]
    pub fn review(&mut self) -> Result<Review> {
        self.review_source(None)
    }

    pub fn review_source(&mut self, number: Option<u64>) -> Result<Review> {
        if let Some(number) = number.or(self.default_pr).filter(|number| *number != 0) {
            return self.review_pr(number);
        }
        let oid = self.resolve_base()?;
        self.ensure_base_index(&oid)?;
        let index = &self.index.as_ref().expect("initialized base index").path;
        let mut files = self.changed_files(index)?;
        for file in &mut files {
            file.patch = self.local_patch(file, index, 3)?;
            if let Some(delta) = file.line_delta() {
                let new = if file.status == "D" {
                    0
                } else {
                    let path = self.root.join(&file.path);
                    self.validate_worktree_path(&file.path)?;
                    let metadata = fs::symlink_metadata(&path)
                        .map_err(|error| format!("Cannot inspect {:?}: {error}", file.path))?;
                    if metadata.file_type().is_symlink() {
                        let target = fs::read_link(&path).map_err(|error| {
                            format!("Cannot read symlink {:?}: {error}", file.path)
                        })?;
                        count_lines(target.as_os_str().as_encoded_bytes())
                    } else if metadata.is_file() {
                        let input = fs::File::open(&path)
                            .map_err(|error| format!("Cannot read {:?}: {error}", file.path))?;
                        count_reader_lines(io::BufReader::new(input)).map_err(|error| {
                            format!("Cannot count lines in {:?}: {error}", file.path)
                        })?
                    } else {
                        continue;
                    }
                };
                file.set_line_totals(new, delta)?;
            }
        }
        let mut branch = command(&self.root);
        branch.args(["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let branch = branch
            .output()
            .map_err(|error| format!("Cannot read branch: {error}"))?;
        let branch = if branch.status.success() {
            line(branch.stdout, "Branch name")?
        } else if branch.status.code() == Some(1) {
            "(detached HEAD)".into()
        } else {
            return Err(git_error(&branch));
        };
        Ok(Review {
            repo: self.root.to_str().expect("validated root").into(),
            base: self.base.clone(),
            base_oid: oid,
            branch,
            files,
            source: "local",
            head_oid: None,
            pr_url: None,
            title: None,
        })
    }

    #[cfg(test)]
    pub fn diff(&mut self, path: &str, context: u32) -> Result<String> {
        self.diff_source(path, context, None)
    }

    pub fn diff_source(&mut self, path: &str, context: u32, number: Option<u64>) -> Result<String> {
        if context > 1_000_000 {
            return Err("Context must be an integer between 0 and 1000000".into());
        }
        validate_relative(path)?;
        if let Some(number) = number.or(self.default_pr).filter(|number| *number != 0) {
            return self.diff_pr(number, path, context);
        }
        let oid = self.resolve_base()?;
        self.ensure_base_index(&oid)?;
        let index = &self.index.as_ref().expect("initialized base index").path;
        let files = self.changed_files(index)?;
        let file = files
            .iter()
            .find(|file| file.path == path)
            .ok_or("This path is no longer changed. Refresh the file list.")?;
        self.local_patch(file, index, context)
    }

    fn local_patch(&self, file: &ChangedFile, index: &Path, context: u32) -> Result<String> {
        let path = &file.path;
        self.validate_worktree_path(path)?;
        let untracked = file.status == "A";
        let mut diff = command(&self.root);
        diff.env("GIT_INDEX_FILE", index);
        diff.args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--no-color",
        ]);
        diff.arg(format!("--unified={context}"));
        if untracked {
            // --no-index compares symlink contents, not their target, just like ordinary Git diffs.
            fs::symlink_metadata(self.root.join(path))
                .map_err(|error| format!("Cannot read {path:?}; refresh the file list: {error}"))?;
            diff.args(["--no-index", "--", "/dev/null", path]);
        } else {
            diff.args(["--", path]);
        }
        let output = diff
            .output()
            .map_err(|error| format!("Cannot run Git diff: {error}"))?;
        if !output.status.success() && !(untracked && output.status.code() == Some(1)) {
            return Err(git_error(&output));
        }
        // Git emits raw file content. Lossy decoding keeps non-UTF-8 text viewable;
        // Git's own binary detection still supplies the binary-file notice.
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn validate_worktree_path(&self, path: &str) -> Result<()> {
        let mut current = self.root.clone();
        let mut components = Path::new(path).components().peekable();
        while let Some(component) = components.next() {
            current.push(component.as_os_str());
            match fs::symlink_metadata(&current) {
                Ok(metadata)
                    if components.peek().is_some() && metadata.file_type().is_symlink() =>
                {
                    return Err(format!("Cannot diff {path:?}: a parent directory is a symlink. Refresh the file list."));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(error) => return Err(format!("Cannot inspect {path:?}: {error}")),
            }
        }
        Ok(())
    }
}

fn validate_relative(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains('\0')
        || Path::new(path).components().any(
            |component| !matches!(component, Component::Normal(name) if name != OsStr::new(".git")),
        )
    {
        return Err("Invalid repository-relative file path".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

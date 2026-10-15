use super::*;
use serde::Deserialize;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    number: u64,
    url: String,
    title: String,
    base_ref_name: String,
    base_ref_oid: String,
    head_ref_oid: String,
}

pub(super) struct PullRequest {
    directory: tempfile::TempDir,
    metadata: Metadata,
    remote: String,
    merge_base: String,
}

fn github_command(root: &Path) -> Command {
    let mut command = Command::new("gh");
    command
        .current_dir(root)
        .env_remove("GH_REPO")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("GH_PROMPT_DISABLED", "1");
    command
}

fn checked_input(mut command: Command, input: &str) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot run Git: {error}"))?;
    let mut stdin = child.stdin.take().ok_or("Cannot open Git input")?;
    // Drain output while feeding input, even for PRs large enough to fill a pipe.
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input.as_bytes()));
        let output = child
            .wait_with_output()
            .map_err(|error| format!("Cannot read Git output: {error}"))?;
        let written = writer.join().map_err(|_| "Cannot write Git input")?;
        if !output.status.success() {
            return Err(git_error(&output));
        }
        written.map_err(|error| format!("Cannot write Git input: {error}"))?;
        Ok(output.stdout)
    })
}

impl Metadata {
    fn load(root: &Path, number: u64) -> Result<Self> {
        let output = github_command(root)
            .args([
                "pr",
                "view",
                &number.to_string(),
                "--json",
                "number,url,title,baseRefName,baseRefOid,headRefOid",
            ])
            .output()
            .map_err(|error| {
                format!("Cannot run GitHub CLI (gh is required for PR reviews): {error}")
            })?;
        if !output.status.success() {
            return Err(format!(
                "Cannot resolve PR #{number}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let metadata: Self = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("Cannot read GitHub PR metadata: {error}"))?;
        if metadata.number != number
            || !valid_oid(&metadata.base_ref_oid)
            || !valid_oid(&metadata.head_ref_oid)
        {
            return Err("GitHub returned invalid PR commit metadata".into());
        }
        let mut validate = command(root);
        validate.args([
            "check-ref-format",
            &format!("refs/heads/{}", metadata.base_ref_name),
        ]);
        checked(validate).map_err(|error| format!("Invalid PR base branch: {error}"))?;
        Ok(metadata)
    }

    fn repository(&self) -> Result<(&str, &str, &str)> {
        let invalid = "GitHub returned an invalid PR URL";
        let (host, path) = self
            .url
            .strip_prefix("https://")
            .and_then(|url| url.split_once('/'))
            .ok_or(invalid)?;
        let hostname = if let Some((hostname, port)) = host.split_once(':') {
            if port.parse::<u16>().ok().filter(|port| *port != 0).is_none() {
                return Err(invalid.into());
            }
            hostname
        } else {
            host
        };
        if hostname.split('.').any(|label| {
            label.is_empty()
                || !label.starts_with(|character: char| character.is_ascii_alphanumeric())
                || !label.ends_with(|character: char| character.is_ascii_alphanumeric())
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        }) {
            return Err(invalid.into());
        }
        let mut parts = path.split('/');
        let owner = parts.next().ok_or(invalid)?;
        let repository = parts.next().ok_or(invalid)?;
        if parts.next() != Some("pull")
            || parts.next() != Some(self.number.to_string().as_str())
            || parts.next().is_some()
            || [owner, repository].iter().any(|part| {
                part.is_empty()
                    || matches!(*part, "." | "..")
                    || !part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
            })
        {
            return Err(invalid.into());
        }
        Ok((host, owner, repository))
    }

    fn remote(&self) -> Result<String> {
        let (host, owner, repository) = self.repository()?;
        Ok(format!("https://{host}/{owner}/{repository}.git"))
    }

    fn merge_base(&self, root: &Path) -> Result<String> {
        let (host, owner, repository) = self.repository()?;
        let endpoint = format!(
            "repos/{owner}/{repository}/compare/{}...{}",
            self.base_ref_oid, self.head_ref_oid
        );
        let output = github_command(root)
            .args([
                "api",
                "--hostname",
                host,
                &endpoint,
                "--jq",
                ".merge_base_commit.sha",
            ])
            .output()
            .map_err(|error| format!("Cannot run GitHub CLI to resolve PR merge base: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "Cannot resolve merge base for PR #{} (check GitHub access with gh auth status): {}",
                self.number,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let oid = line(output.stdout, "GitHub merge base")?;
        if !valid_oid(&oid) {
            return Err("GitHub returned an invalid PR merge-base commit".into());
        }
        Ok(oid)
    }
}

fn valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl PullRequest {
    fn fetch(
        target: &Path,
        metadata: Metadata,
        remote: String,
        merge_base: String,
    ) -> Result<Self> {
        if !valid_oid(&merge_base)
            || !valid_oid(&metadata.base_ref_oid)
            || !valid_oid(&metadata.head_ref_oid)
        {
            return Err("Invalid PR snapshot commit".into());
        }
        let directory = tempfile::tempdir()
            .map_err(|error| format!("Cannot create isolated PR repository: {error}"))?;
        let mut init = command(directory.path());
        init.args(["init", "--bare", "--template="]);
        if metadata.head_ref_oid.len() == 64 {
            init.arg("--object-format=sha256");
        }
        checked(init)?;

        // Borrow only objects, never the checkout's refs, index, or shallow
        // boundary. --git-path resolves the shared store for linked worktrees.
        let mut objects = command(target);
        objects.args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ]);
        let objects = PathBuf::from(line(checked(objects)?, "Target object directory")?);
        let objects = fs::canonicalize(objects)
            .map_err(|error| format!("Cannot locate target Git objects: {error}"))?;
        let objects = objects
            .to_str()
            .ok_or("Target Git object path is not valid UTF-8")?;
        if objects.contains(['\n', '\r']) {
            return Err("Target Git object path contains a newline".into());
        }
        fs::write(
            directory.path().join("objects/info/alternates"),
            format!("{objects}\n"),
        )
        .map_err(|error| format!("Cannot reuse target Git objects: {error}"))?;

        // Git's promisor-remote configuration enables missing-blob fetching
        // for ordinary diff commands, including in a bare repository. Persist
        // authentication too: those internal fetches do not use our argv.
        for (key, value) in [
            ("remote.origin.url", remote.as_str()),
            ("remote.origin.promisor", "true"),
            ("remote.origin.partialclonefilter", "blob:none"),
            ("credential.helper", ""),
            ("fetch.negotiationAlgorithm", "noop"),
            ("gc.auto", "0"),
            ("maintenance.auto", "false"),
        ] {
            let mut config = command(directory.path());
            config.args(["config", "--local", key, value]);
            checked(config)?;
        }
        let mut credentials = command(directory.path());
        credentials.args([
            "config",
            "--local",
            "--add",
            "credential.helper",
            "!gh auth git-credential",
        ]);
        checked(credentials)?;

        let snapshots = [
            ("refs/review/base", merge_base.as_str()),
            ("refs/review/head", metadata.head_ref_oid.as_str()),
        ];
        let mut missing = Vec::new();
        for (reference, oid) in snapshots {
            if !Self::has_commit(directory.path(), oid)? {
                missing.push(format!("+{oid}:{reference}"));
            }
        }
        if !missing.is_empty() {
            let mut fetch = command(directory.path());
            fetch.args([
                "fetch",
                "--depth=1",
                "--filter=blob:none",
                "--no-tags",
                "--no-recurse-submodules",
                "--no-write-fetch-head",
                "--",
                "origin",
            ]);
            fetch.args(missing);
            checked(fetch).map_err(|error| {
                format!(
                    "Cannot fetch snapshots for PR #{} (check GitHub access with gh auth status): {error}",
                    metadata.number
                )
            })?;
        }
        for (reference, oid) in snapshots {
            if !Self::has_commit(directory.path(), oid)? {
                return Err(format!("Git did not provide PR snapshot {oid}"));
            }
            let mut update = command(directory.path());
            update.args(["update-ref", reference, oid]);
            checked(update)?;
        }
        Ok(Self {
            directory,
            metadata,
            remote,
            merge_base,
        })
    }

    fn has_commit(root: &Path, oid: &str) -> Result<bool> {
        let mut probe = command(root);
        // cat-file normally invokes the promisor remote for missing objects.
        // Probing must stay local, particularly for shallow borrowed history.
        probe.env("GIT_NO_LAZY_FETCH", "1");
        probe.args(["cat-file", "-e", &format!("{oid}^{{commit}}")]);
        let output = probe
            .output()
            .map_err(|error| format!("Cannot inspect PR snapshot objects: {error}"))?;
        Ok(output.status.success())
    }

    fn files(&self) -> Result<Vec<(ChangedFile, Option<String>)>> {
        let mut diff = command(self.directory.path());
        diff.args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--no-color",
            "--raw",
            "--no-abbrev",
            "-z",
            &self.merge_base,
            &self.metadata.head_ref_oid,
            "--",
        ]);
        let bytes = checked(diff)?;
        let mut fields = bytes.split(|byte| *byte == 0);
        let mut files = Vec::new();
        let mut blobs = std::collections::BTreeSet::new();
        while let Some(header) = fields.next().filter(|header| !header.is_empty()) {
            let header = std::str::from_utf8(header).map_err(|_| "Invalid PR diff header")?;
            let mut columns = header.split(' ');
            let invalid = "Git returned an invalid PR diff header";
            let old_mode = columns
                .next()
                .and_then(|mode| mode.strip_prefix(':'))
                .ok_or(invalid)?;
            let new_mode = columns.next().ok_or(invalid)?;
            let old_oid = columns.next().ok_or(invalid)?;
            let new_oid = columns.next().ok_or(invalid)?;
            let status = columns.next().ok_or(invalid)?;
            if columns.next().is_some() {
                return Err(invalid.into());
            }
            for (mode, oid) in [(old_mode, old_oid), (new_mode, new_oid)] {
                if !valid_oid(oid) {
                    return Err("Git returned an invalid PR blob ID".into());
                }
                if mode != "160000" && oid.bytes().any(|byte| byte != b'0') {
                    blobs.insert(oid);
                }
            }
            let path = fields
                .next()
                .ok_or("Git returned an incomplete PR file status")?;
            let path = text(path.to_vec(), "Changed filename")?;
            validate_relative(&path)?;
            let new_blob = (new_mode != "160000" && new_oid.bytes().any(|byte| byte != b'0'))
                .then(|| new_oid.to_owned());
            files.push((
                ChangedFile {
                    path,
                    status: status.to_owned(),
                    patch: String::new(),
                    old_lines: None,
                    new_lines: None,
                },
                new_blob,
            ));
        }
        self.materialize_blobs(blobs)?;
        Ok(files)
    }

    fn materialize_blobs(&self, blobs: std::collections::BTreeSet<&str>) -> Result<()> {
        if blobs.is_empty() {
            return Ok(());
        }
        let mut input = String::new();
        for oid in &blobs {
            input.push_str(oid);
            input.push('\n');
        }
        let mut probe = command(self.directory.path());
        probe.env("GIT_NO_LAZY_FETCH", "1");
        probe.args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]);
        let output = text(checked_input(probe, &input)?, "PR blob availability")?;
        let mut missing = String::new();
        for result in output.lines() {
            let (oid, kind) = result
                .split_once(' ')
                .ok_or("Invalid PR blob availability")?;
            if !blobs.contains(oid) {
                return Err("Git returned an unexpected PR blob ID".into());
            }
            match kind {
                "blob" => {}
                "missing" => {
                    missing.push_str(oid);
                    missing.push('\n');
                }
                _ => return Err("Git returned a non-blob for a PR file".into()),
            }
        }
        if !missing.is_empty() {
            // Fetch only explicitly wanted changed blobs in one round trip.
            // Unchanged repository blobs remain absent, even for full context.
            let mut fetch = command(self.directory.path());
            fetch.args([
                "fetch",
                "--filter=blob:none",
                "--no-tags",
                "--no-recurse-submodules",
                "--no-write-fetch-head",
                "--stdin",
                "--",
                "origin",
            ]);
            checked_input(fetch, &missing).map_err(|error| {
                format!(
                    "Cannot load changed files for PR #{} (check GitHub access with gh auth status): {error}",
                    self.metadata.number
                )
            })?;
        }
        Ok(())
    }

    fn blob_line_totals(
        &self,
        blobs: std::collections::BTreeSet<&str>,
    ) -> Result<HashMap<String, usize>> {
        use std::io::{BufRead, Read, Write};
        use std::process::Stdio;

        if blobs.is_empty() {
            return Ok(HashMap::new());
        }
        let mut input = String::new();
        for oid in &blobs {
            input.push_str(oid);
            input.push('\n');
        }
        let mut batch = command(self.directory.path());
        // files() already materialized exactly the changed blobs in one fetch.
        batch.env("GIT_NO_LAZY_FETCH", "1");
        batch.args(["cat-file", "--batch"]);
        let mut child = batch
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Cannot count PR file lines: {error}"))?;
        let mut stdin = child.stdin.take().ok_or("Cannot open Git input")?;
        let stdout = child.stdout.take().ok_or("Cannot open Git output")?;
        let mut stderr = child.stderr.take().ok_or("Cannot open Git error output")?;
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || stdin.write_all(input.as_bytes()));
            let errors = scope.spawn(move || {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).map(|_| bytes)
            });
            let totals = (|| -> Result<HashMap<String, usize>> {
                let mut reader = io::BufReader::new(stdout);
                let mut header = String::new();
                let mut totals = HashMap::with_capacity(blobs.len());
                for expected in &blobs {
                    header.clear();
                    reader
                        .read_line(&mut header)
                        .map_err(|error| format!("Cannot read PR blob header: {error}"))?;
                    let mut fields = header.split_whitespace();
                    if fields.next() != Some(*expected) || fields.next() != Some("blob") {
                        return Err("Git returned an invalid PR blob header".into());
                    }
                    let length = fields
                        .next()
                        .and_then(|size| size.parse::<u64>().ok())
                        .ok_or("Git returned an invalid PR blob size")?;
                    if fields.next().is_some() {
                        return Err("Git returned an invalid PR blob header".into());
                    }
                    // Take borrows the existing read buffer; no blob-sized
                    // allocation and no second read of the old file is needed.
                    let lines = count_reader_lines((&mut reader).take(length))
                        .map_err(|error| format!("Cannot count PR blob lines: {error}"))?;
                    let mut delimiter = [0];
                    reader
                        .read_exact(&mut delimiter)
                        .map_err(|error| format!("Cannot read PR blob delimiter: {error}"))?;
                    if delimiter != [b'\n'] {
                        return Err("Git returned an invalid PR blob delimiter".into());
                    }
                    totals.insert((*expected).to_owned(), lines);
                }
                Ok(totals)
            })();
            if totals.is_err() {
                let _ = child.kill();
            }
            let status = child
                .wait()
                .map_err(|error| format!("Cannot wait for Git: {error}"))?;
            let written = writer.join().map_err(|_| "Cannot write Git input")?;
            let errors = errors
                .join()
                .map_err(|_| "Cannot read Git errors")?
                .map_err(|error| format!("Cannot read Git errors: {error}"))?;
            if !status.success() {
                return Err(git_error(&Output {
                    status,
                    stdout: Vec::new(),
                    stderr: errors,
                }));
            }
            written.map_err(|error| format!("Cannot write Git input: {error}"))?;
            totals
        })
    }

    fn diff_command(&self, context: u32) -> Command {
        let mut diff = command(self.directory.path());
        diff.args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--no-color",
        ]);
        diff.arg(format!("--unified={context}"));
        diff
    }

    fn patch(&self, path: &str, context: u32) -> Result<String> {
        let mut diff = self.diff_command(context);
        diff.args([
            self.merge_base.as_str(),
            self.metadata.head_ref_oid.as_str(),
            "--",
            path,
        ]);
        Ok(String::from_utf8_lossy(&checked(diff)?).into_owned())
    }

    fn review(&self, repo: String) -> Result<Review> {
        let mut snapshots = self.files()?;
        let mut blobs = std::collections::BTreeSet::new();
        for (file, new_blob) in &mut snapshots {
            file.patch = self.patch(&file.path, 3)?;
            if file.line_delta().is_some() {
                if let Some(oid) = new_blob.as_deref() {
                    blobs.insert(oid);
                }
            }
        }
        let totals = self.blob_line_totals(blobs)?;
        let mut files = Vec::with_capacity(snapshots.len());
        for (mut file, new_blob) in snapshots {
            if let Some(delta) = file.line_delta() {
                let new = match new_blob {
                    Some(oid) => *totals.get(&oid).ok_or("Missing PR file line total")?,
                    None => 0,
                };
                file.set_line_totals(new, delta)?;
            }
            files.push(file);
        }
        Ok(Review {
            repo,
            base: self.metadata.base_ref_name.clone(),
            base_oid: self.merge_base.clone(),
            branch: format!("PR #{}", self.metadata.number),
            files,
            source: "pr",
            head_oid: Some(self.metadata.head_ref_oid.clone()),
            pr_url: Some(self.metadata.url.clone()),
            title: Some(self.metadata.title.clone()),
        })
    }
}

impl Repository {
    fn pull_request(&mut self, number: u64) -> Result<&PullRequest> {
        // Resolve on every request so a refresh observes force-pushes and base
        // branch changes. Only unchanged Git objects are cached, never selection.
        let metadata = Metadata::load(&self.root, number)?;
        let remote = metadata.remote()?;
        let unchanged = self.pull_requests.get(&number).is_some_and(|cached| {
            cached.metadata.base_ref_oid == metadata.base_ref_oid
                && cached.metadata.head_ref_oid == metadata.head_ref_oid
                && cached.remote == remote
        });
        if unchanged {
            self.pull_requests
                .get_mut(&number)
                .expect("cached PR")
                .metadata = metadata;
        } else {
            let merge_base = metadata.merge_base(&self.root)?;
            let pr = PullRequest::fetch(&self.root, metadata, remote, merge_base)?;
            self.pull_requests.insert(number, pr);
        }
        Ok(self.pull_requests.get(&number).expect("initialized PR"))
    }

    pub(super) fn review_pr(&mut self, number: u64) -> Result<Review> {
        let repo = self.root.to_str().expect("validated root").to_owned();
        self.pull_request(number)?.review(repo)
    }

    pub(super) fn diff_pr(&mut self, number: u64, path: &str, context: u32) -> Result<String> {
        let pr = self.pull_request(number)?;
        let patch = pr.patch(path, context)?;
        if patch.is_empty() {
            return Err("This path is no longer changed. Refresh the file list.".into());
        }
        Ok(patch)
    }
}

#[cfg(test)]
mod tests;

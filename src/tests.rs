use super::*;
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::Path,
    process::Command,
    sync::mpsc,
    time::Duration,
};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let fixture = Self {
            _directory: directory,
            root,
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "Review Test"]);
        fixture.git(&["config", "user.email", "review@example.invalid"]);
        fixture.write("file.txt", "base\none\ntwo\nthree\nfour\nfive\n");
        fixture.commit("base");
        fixture.git(&["tag", "baseline"]);
        fixture
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", message]);
    }

    fn state(&self, base: &str, pr: Option<u64>) -> Shared {
        Arc::new(
            repositories::Repositories::new(
                Some(&self.root),
                base.into(),
                pr,
                self.root.clone(),
                None,
            )
            .unwrap(),
        )
    }
}

struct Server {
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn new(state: Shared) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app(state, vec!["review.local".into()]))
                .await
                .unwrap();
        });
        Self { address, task }
    }

    async fn get(&self, path: String) -> (u16, Value) {
        let address = self.address;
        tokio::task::spawn_blocking(move || request(address, &path))
            .await
            .unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn request(address: SocketAddr, path: &str) -> (u16, Value) {
    request_headers(address, path, &format!("Host: {address}\r\n"))
}

fn request_headers(address: SocketAddr, path: &str, headers: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\n{headers}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let boundary = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap();
    let headers = std::str::from_utf8(&response[..boundary]).unwrap();
    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(headers
        .to_ascii_lowercase()
        .contains("cache-control: no-store"));
    let body = serde_json::from_slice(&response[boundary + 4..]).unwrap();
    (status, body)
}

fn encoded(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(output, "%{byte:02X}").unwrap();
        }
    }
    output
}

fn repo_query(root: &Path) -> String {
    format!("repo={}", encoded(root.to_str().unwrap()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_requests_cannot_rebind_or_cross_origins_to_read_repositories() {
    let fixture = Fixture::new();
    let server = Server::new(fixture.state("baseline", None)).await;
    let address = server.address;
    tokio::task::spawn_blocking(move || {
        for host in [
            "localhost:4173",
            "127.0.0.1:4173",
            "[::1]:4173",
            "review.local",
        ] {
            let (status, body) =
                request_headers(address, "/api/config", &format!("Host: {host}\r\n"));
            assert_eq!(status, 200, "{host}: {body}");
            assert!(body["repo"].is_string());
        }
        for headers in [
            "Host: attacker.example\r\n",
            "Host: localhost.attacker.example\r\n",
            "Host: attacker.example\r\nX-Forwarded-Host: localhost\r\n",
            "Host: localhost\r\nOrigin: https://attacker.example\r\n",
            "Host: localhost\r\nOrigin: null\r\n",
            "Host: localhost\r\nSec-Fetch-Site: cross-site\r\n",
            "Host: review.local\r\nOrigin: https://other.local\r\n",
        ] {
            let (status, body) = request_headers(address, "/api/config", headers);
            assert_eq!(status, 403, "{headers}: {body}");
            assert!(body.get("repo").is_none());
        }
        let (status, body) = request_headers(
            address,
            "/api/config",
            "Host: review.local\r\nOrigin: https://review.local\r\nSec-Fetch-Site: same-origin\r\n",
        );
        assert_eq!(status, 200, "{body}");
        assert!(body["repo"].is_string());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_repositories_and_bases_keep_review_and_context_consistent() {
    let a = Fixture::new();
    let b = Fixture::new();
    a.write("file.txt", "committed A\none\ntwo\nthree\nfour\nfive\n");
    a.commit("advance A");
    a.git(&["tag", "alternate"]);
    a.write("file.txt", "working A\none\ntwo\nthree\nfour\nfive\n");
    b.write("file.txt", "working B\none\ntwo\nthree\nfour\nfive\n");
    let state = a.state("baseline", None);
    let server = Server::new(state.clone()).await;
    let index_a = fs::read(a.root.join(".git/index")).unwrap();
    let index_b = fs::read(b.root.join(".git/index")).unwrap();
    let (status, review_a) = server
        .get(format!("/api/review?{}", repo_query(&a.root)))
        .await;
    assert_eq!(status, 200);
    assert_eq!(review_a["repo"], a.root.to_str().unwrap());
    assert_eq!(review_a["source"], "local");
    assert_eq!(review_a["base"], "baseline");
    assert_eq!(review_a["files"][0]["old_lines"], 6);
    assert_eq!(review_a["files"][0]["new_lines"], 6);
    assert!(review_a["files"][0]["patch"]
        .as_str()
        .unwrap()
        .contains("+working A\n"));
    let (status, review_b) = server
        .get(format!("/api/review?{}", repo_query(&b.root)))
        .await;
    assert_eq!(status, 200);
    assert_eq!(review_b["repo"], b.root.to_str().unwrap());
    assert!(review_b["files"][0]["patch"]
        .as_str()
        .unwrap()
        .contains("+working B\n"));
    let query = format!("{}&base=alternate", repo_query(&a.root));
    let (status, alternate) = server.get(format!("/api/review?{query}")).await;
    assert_eq!(status, 200);
    let patch = alternate["files"][0]["patch"].as_str().unwrap();
    assert!(patch.contains("-committed A\n"));
    assert!(patch.contains("+working A\n"));
    let (status, expanded) = server
        .get(format!("/api/diff?{query}&path=file.txt&context=100"))
        .await;
    assert_eq!(status, 200);
    let expanded = expanded["patch"].as_str().unwrap();
    assert!(expanded.contains("-committed A\n"));
    assert!(expanded.contains(" five\n"));
    let (status, patch_b) = server
        .get(format!(
            "/api/diff?{}&path=file.txt&context=100",
            repo_query(&b.root)
        ))
        .await;
    assert_eq!(status, 200);
    assert!(patch_b["patch"].as_str().unwrap().contains("+working B\n"));
    let (status, default) = server.get("/api/review".into()).await;
    assert_eq!(status, 200);
    assert_eq!(default["repo"], review_a["repo"]);
    assert_eq!(default["base_oid"], review_a["base_oid"]);
    assert_eq!(default["files"], review_a["files"]);
    assert_eq!(index_a, fs::read(a.root.join(".git/index")).unwrap());
    assert_eq!(index_b, fs::read(b.root.join(".git/index")).unwrap());
    let (status, error) = server
        .get(format!("/api/review?{}&base=missing", repo_query(&b.root)))
        .await;
    assert_eq!(status, 409);
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("Cannot resolve base"));
    assert!(!Arc::ptr_eq(
        &state.select(None, None).unwrap(),
        &state.select(None, Some("alternate")).unwrap(),
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subdirectories_and_symlinks_reuse_workers_but_worktrees_are_distinct() {
    let fixture = Fixture::new();
    fixture.write("nested/file", "nested\n");
    let state = fixture.state("main", None);
    let worker = state.select(None, None).unwrap();
    let nested = fixture.root.join("nested");
    assert!(Arc::ptr_eq(
        &worker,
        &state.select(nested.to_str(), None).unwrap()
    ));
    let siblings = tempfile::tempdir().unwrap();
    let worktree = siblings.path().join("worktree");
    fixture.git(&[
        "worktree",
        "add",
        "-b",
        "other",
        worktree.to_str().unwrap(),
        "main",
    ]);
    fs::write(worktree.join("file.txt"), "worktree only\n").unwrap();
    assert!(!Arc::ptr_eq(
        &worker,
        &state.select(worktree.to_str(), None).unwrap()
    ));
    let server = Server::new(state.clone()).await;
    let (status, nested_review) = server
        .get(format!("/api/review?{}", repo_query(&nested)))
        .await;
    assert_eq!(status, 200);
    assert_eq!(nested_review["repo"], fixture.root.to_str().unwrap());
    let (status, nested_listing) = server
        .get(format!(
            "/api/repos?path={}",
            encoded(nested.to_str().unwrap())
        ))
        .await;
    assert_eq!(status, 200);
    assert_eq!(nested_listing["repo_root"], fixture.root.to_str().unwrap());
    let (status, review) = server
        .get(format!("/api/review?{}", repo_query(&worktree)))
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        review["repo"],
        fs::canonicalize(&worktree).unwrap().to_str().unwrap()
    );
    assert!(review["files"][0]["patch"]
        .as_str()
        .unwrap()
        .contains("+worktree only\n"));
    let (status, listing) = server
        .get(format!(
            "/api/repos?path={}",
            encoded(siblings.path().to_str().unwrap())
        ))
        .await;
    assert_eq!(status, 200);
    assert_eq!(listing["directories"][0]["is_repo"], true);
    #[cfg(unix)]
    {
        let link = siblings.path().join("alias");
        std::os::unix::fs::symlink(&fixture.root, &link).unwrap();
        assert!(Arc::ptr_eq(
            &worker,
            &state.select(link.to_str(), None).unwrap()
        ));
        let (status, alias) = server
            .get(format!("/api/review?{}", repo_query(&link)))
            .await;
        assert_eq!(status, 200);
        assert_eq!(alias["repo"], fixture.root.to_str().unwrap());
        let (status, listing) = server
            .get(format!(
                "/api/repos?path={}",
                encoded(link.to_str().unwrap())
            ))
            .await;
        assert_eq!(status, 200);
        assert_eq!(listing["path"], fixture.root.to_str().unwrap());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_and_other_repositories_do_not_wait_for_a_busy_worker() {
    let a = Fixture::new();
    let b = Fixture::new();
    b.write("file.txt", "B\n");
    let state = a.state("baseline", None);
    let worker = state.select(None, None).unwrap();
    let guard = match worker.lock() {
        Ok(guard) => guard,
        Err(error) => panic!("Cannot hold repository worker for contention test: {error}"),
    };
    let server = Server::new(state).await;
    let address = server.address;
    let query = format!("/api/review?{}", repo_query(&b.root));
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let config = request(address, "/api/config");
        let review = request(address, &query);
        sender.send((config, review)).unwrap();
    });
    let ((status, config), (review_status, review)) =
        receiver.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(guard);
    assert_eq!(status, 200);
    assert_eq!(config["repo"], a.root.to_str().unwrap());
    assert_eq!(review_status, 200);
    assert_eq!(review["repo"], b.root.to_str().unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_repo_startup_config_and_folder_navigation_handle_real_paths() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    fs::create_dir(root.join("zeta")).unwrap();
    fs::create_dir(root.join("alpha")).unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    fs::write(root.join("regular-file"), "not a directory").unwrap();
    // A .git directory alone is not a valid repository and is still hidden.
    let state = Arc::new(
        repositories::Repositories::new(
            None,
            "main".into(),
            None,
            root.clone(),
            Some(root.clone()),
        )
        .unwrap(),
    );
    let server = Server::new(state).await;
    let (status, config) = server.get("/api/config".into()).await;
    assert_eq!(status, 200);
    assert!(config["repo"].is_null());
    assert_eq!(config["base"], "main");
    assert!(config["pr"].is_null());
    assert_eq!(config["browse_path"], root.to_str().unwrap());
    let (status, error) = server.get("/api/review".into()).await;
    assert_eq!(status, 409);
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("Choose a repository"));
    let (status, listing) = server.get("/api/repos".into()).await;
    assert_eq!(status, 200);
    assert_eq!(listing["path"], root.to_str().unwrap());
    assert_eq!(listing["parent"], root.parent().unwrap().to_str().unwrap());
    assert!(listing["repo_root"].is_null());
    let names: Vec<_> = listing["directories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|directory| directory["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["alpha", "zeta"]);
    assert_eq!(listing["directories"][0]["is_repo"], false);
    let (status, home) = server.get("/api/repos?path=~".into()).await;
    assert_eq!(status, 200);
    assert_eq!(home, listing);
    let (status, child) = server.get("/api/repos?path=~%2Falpha".into()).await;
    assert_eq!(status, 200);
    assert_eq!(child["path"], root.join("alpha").to_str().unwrap());
    for invalid in ["missing", "regular-file"] {
        let (status, error) = server.get(format!("/api/repos?path={invalid}")).await;
        assert_eq!(status, 409);
        assert!(error["error"].is_string());
    }
    let (status, error) = server.get("/api/repos?unexpected=1".into()).await;
    assert_eq!(status, 400);
    assert!(error["error"].is_string());
    let fixture = Fixture::new();
    let (status, selected) = server
        .get(format!("/api/review?{}", repo_query(&fixture.root)))
        .await;
    assert_eq!(status, 200);
    assert_eq!(selected["repo"], fixture.root.to_str().unwrap());
    let (status, config_after) = server.get("/api/config".into()).await;
    assert_eq!(status, 200);
    assert_eq!(config_after, config);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_discovery_defers_missing_base_and_pr_zero_overrides_cli_default() {
    let fixture = Fixture::new();
    fixture.git(&["branch", "-m", "topic"]);
    fixture.write("file.txt", "local\n");
    let state = Arc::new(
        repositories::Repositories::new(None, "main".into(), None, fixture.root.clone(), None)
            .unwrap(),
    );
    let server = Server::new(state).await;
    let (status, config) = server.get("/api/config".into()).await;
    assert_eq!(status, 200);
    assert_eq!(config["repo"], fixture.root.to_str().unwrap());
    assert_eq!(
        config["browse_path"],
        fixture.root.parent().unwrap().to_str().unwrap()
    );
    let (status, error) = server.get("/api/review".into()).await;
    assert_eq!(status, 409);
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("Cannot resolve base"));
    let (status, corrected) = server.get("/api/review?base=topic".into()).await;
    assert_eq!(status, 200);
    assert_eq!(corrected["base"], "topic");
    assert_eq!(corrected["source"], "local");
    let (status, patch) = server
        .get("/api/diff?base=topic&path=file.txt&context=100".into())
        .await;
    assert_eq!(status, 200);
    assert!(patch["patch"].as_str().unwrap().contains("+local\n"));
    assert!(repositories::Repositories::new(
        Some(&fixture.root),
        "main".into(),
        None,
        fixture.root.clone(),
        None,
    )
    .is_err());
    let outside = tempfile::tempdir().unwrap();
    assert!(repositories::Repositories::new(
        Some(outside.path()),
        "main".into(),
        None,
        fixture.root.clone(),
        None,
    )
    .is_err());
    let pr_server = Server::new(fixture.state("missing", Some(7))).await;
    let (status, config) = pr_server.get("/api/config".into()).await;
    assert_eq!(status, 200);
    assert_eq!(config["pr"], 7);
    let (status, review) = pr_server.get("/api/review?base=topic&pr=0".into()).await;
    assert_eq!(status, 200);
    assert_eq!(review["source"], "local");
    let (status, patch) = pr_server
        .get("/api/diff?base=topic&pr=0&path=file.txt".into())
        .await;
    assert_eq!(status, 200);
    assert!(patch["patch"].as_str().unwrap().contains("+local\n"));
    let (status, config) = pr_server.get("/api/config".into()).await;
    assert_eq!(status, 200);
    assert_eq!(config["pr"], 7);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_directory_is_a_json_error_when_permissions_are_enforced() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    let state = Arc::new(
        repositories::Repositories::new(None, "main".into(), None, root.clone(), None).unwrap(),
    );
    let locked = root.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0)).unwrap();
    // Root can bypass mode bits; do not pretend it is an unreadable path there.
    let inaccessible = fs::read_dir(&locked).is_err();
    let server = Server::new(state).await;
    let (status, body) = server
        .get(format!(
            "/api/repos?path={}",
            encoded(locked.to_str().unwrap())
        ))
        .await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    if inaccessible {
        assert_eq!(status, 409);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("Cannot read directory"));
    } else {
        assert_eq!(status, 200);
    }
}

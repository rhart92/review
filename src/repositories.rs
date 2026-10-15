use crate::git;
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub type Worker = Arc<Mutex<git::Repository>>;

#[derive(Serialize)]
pub struct Config {
    pub repo: Option<String>,
    pub base: String,
    pub pr: Option<u64>,
    pub browse_path: String,
}

#[derive(Default)]
struct Registry {
    roots: HashMap<PathBuf, PathBuf>,
    workers: HashMap<(PathBuf, String), Worker>,
}

pub struct Repositories {
    pub config: Config,
    cwd: PathBuf,
    home: Option<PathBuf>,
    registry: Mutex<Registry>,
}

#[derive(Serialize)]
pub struct Directory {
    name: String,
    path: String,
    is_repo: bool,
}

#[derive(Serialize)]
pub struct Listing {
    path: String,
    parent: Option<String>,
    repo_root: Option<String>,
    directories: Vec<Directory>,
}

fn path_text(path: &Path) -> git::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("Path is not valid UTF-8: {}", path.display()))
}

impl Repositories {
    pub fn new(
        repo: Option<&Path>,
        base: String,
        pr: Option<u64>,
        cwd: PathBuf,
        home: Option<PathBuf>,
    ) -> git::Result<Self> {
        let cwd = fs::canonicalize(cwd)
            .map_err(|error| format!("Cannot open current directory: {error}"))?;
        // Explicit CLI selections retain startup validation. Automatic discovery must
        // allow missing default refs so the picker can correct the base or select a PR.
        let initial = repo
            .map(|path| git::Repository::open_with_pr(path, base.clone(), pr))
            .transpose()?;
        let root = match &initial {
            Some(repository) => Some(repository.root().to_path_buf()),
            None => git::discover_root(&cwd)?,
        };
        let browse_path = root.as_deref().and_then(Path::parent).unwrap_or(&cwd);
        let config = Config {
            repo: root.as_deref().map(path_text).transpose()?,
            base: base.clone(),
            pr,
            browse_path: path_text(browse_path)?,
        };
        let mut registry = Registry::default();
        if let Some(root) = root {
            registry.roots.insert(root.clone(), root.clone());
            if let Some(repository) = initial {
                registry
                    .workers
                    .insert((root, base), Arc::new(Mutex::new(repository)));
            }
        }
        Ok(Self {
            config,
            cwd,
            home,
            registry: Mutex::new(registry),
        })
    }

    fn directory(&self, path: &str) -> git::Result<PathBuf> {
        let path = if path == "~" || path.starts_with("~/") {
            let home = self.home.as_ref().ok_or("Home directory is unavailable")?;
            match path.strip_prefix("~/") {
                Some(relative) => home.join(relative),
                None => home.clone(),
            }
        } else {
            self.cwd.join(path)
        };
        let canonical = fs::canonicalize(&path)
            .map_err(|error| format!("Cannot open directory {}: {error}", path.display()))?;
        if !canonical.is_dir() {
            return Err(format!("Not a directory: {}", canonical.display()));
        }
        path_text(&canonical)?;
        Ok(canonical)
    }

    pub fn select(&self, repo: Option<&str>, base: Option<&str>) -> git::Result<Worker> {
        let path = repo
            .or(self.config.repo.as_deref())
            .ok_or("Choose a repository before loading a review")?;
        let directory = self.directory(path)?;
        let cached_root = self
            .registry
            .lock()
            .map_err(|_| "Repository registry lock failed")?
            .roots
            .get(&directory)
            .cloned();
        // Git discovery runs outside the registry lock, including first-time aliases.
        let root = match cached_root {
            Some(root) => root,
            None => git::discover_root(&directory)?
                .ok_or_else(|| format!("Not a Git working tree: {}", directory.display()))?,
        };
        let base = base.unwrap_or(&self.config.base);
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "Repository registry lock failed")?;
        registry.roots.insert(directory, root.clone());
        registry.roots.insert(root.clone(), root.clone());
        Ok(registry
            .workers
            .entry((root.clone(), base.to_owned()))
            .or_insert_with(|| {
                Arc::new(Mutex::new(git::Repository::from_root(
                    root,
                    base.to_owned(),
                    self.config.pr,
                )))
            })
            .clone())
    }

    pub fn browse(&self, path: Option<&str>) -> git::Result<Listing> {
        let path = self.directory(path.unwrap_or(&self.config.browse_path))?;
        let entries = fs::read_dir(&path)
            .map_err(|error| format!("Cannot read directory {}: {error}", path.display()))?;
        let mut directories = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("Cannot read directory {}: {error}", path.display()))?;
            if entry.file_name() == ".git" {
                continue;
            }
            // Follow directory symlinks, but never expose file contents. Broken links
            // and non-directory entries are not navigable picker choices.
            if !entry.path().is_dir() {
                continue;
            }
            let child = fs::canonicalize(entry.path()).map_err(|error| {
                format!("Cannot open directory {}: {error}", entry.path().display())
            })?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Directory name is not valid UTF-8")?;
            directories.push(Directory {
                name,
                path: path_text(&child)?,
                is_repo: child.join(".git").exists(),
            });
        }
        directories.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Listing {
            path: path_text(&path)?,
            parent: path.parent().map(path_text).transpose()?,
            repo_root: git::discover_root(&path)?
                .as_deref()
                .map(path_text)
                .transpose()?,
            directories,
        })
    }
}

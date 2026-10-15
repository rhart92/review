mod git;
mod repositories;
mod security;

use axum::{
    extract::{rejection::QueryRejection, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(
    version,
    about = "Review live working-tree changes against a Git commit in your browser"
)]
struct Cli {
    /// Repository directory (a subdirectory of a working tree is also accepted)
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Base commit or ref; compared directly, not via a merge base
    #[arg(long, default_value = "main")]
    base: String,
    /// Default pushed GitHub pull request; API pr=0 explicitly selects local changes
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pr: Option<u64>,
    /// HTTP bind address; use a non-loopback address only on a trusted network
    #[arg(long, default_value = "127.0.0.1:4173")]
    bind: SocketAddr,
    /// Additional trusted hostname for a private reverse proxy (repeatable; no scheme/port)
    #[arg(long, value_parser = security::parse_host)]
    allow_host: Vec<String>,
}

type Shared = Arc<repositories::Repositories>;

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            [(header::CACHE_CONTROL, "no-store")],
            Json(ErrorBody { error: self.1 }),
        )
            .into_response()
    }
}

impl From<String> for ApiError {
    fn from(error: String) -> Self {
        Self(StatusCode::CONFLICT, error)
    }
}

async fn with_repository<T: Send + 'static>(
    state: Shared,
    repo: Option<String>,
    base: Option<String>,
    operation: impl FnOnce(&mut git::Repository) -> git::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(move || {
        let worker = state.select(repo.as_deref(), base.as_deref())?;
        let mut repository = worker
            .lock()
            .map_err(|_| "Repository worker lock failed".to_string())?;
        operation(&mut repository)
    })
    .await
    .map_err(|error| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Repository worker failed: {error}"),
        )
    })?
    .map_err(ApiError::from)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewQuery {
    repo: Option<String>,
    base: Option<String>,
    pr: Option<u64>,
}

async fn review(
    State(state): State<Shared>,
    query: Result<Query<ReviewQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) =
        query.map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.body_text()))?;
    let review = with_repository(state, query.repo, query.base, move |repository| {
        repository.review_source(query.pr)
    })
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(review)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffQuery {
    repo: Option<String>,
    base: Option<String>,
    path: String,
    #[serde(default = "default_context")]
    context: u32,
    pr: Option<u64>,
}

fn default_context() -> u32 {
    3
}

#[derive(Serialize)]
struct Patch {
    patch: String,
}

async fn diff(
    State(state): State<Shared>,
    query: Result<Query<DiffQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) =
        query.map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.body_text()))?;
    if query.context > 1_000_000 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Context must be an integer between 0 and 1000000".into(),
        ));
    }
    let patch = with_repository(state, query.repo, query.base, move |repository| {
        repository.diff_source(&query.path, query.context, query.pr)
    })
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(Patch { patch })).into_response())
}

async fn config(State(state): State<Shared>) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(&state.config)).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReposQuery {
    path: Option<String>,
}

async fn repos(
    State(state): State<Shared>,
    query: Result<Query<ReposQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) =
        query.map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.body_text()))?;
    let listing = tokio::task::spawn_blocking(move || state.browse(query.path.as_deref()))
        .await
        .map_err(|error| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Directory worker failed: {error}"),
            )
        })?
        .map_err(ApiError::from)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(listing)).into_response())
}

async fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "Not found".into())
}
async fn wrong_method() -> ApiError {
    ApiError(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed".into())
}

fn asset(content: &'static str, kind: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, kind),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        content,
    )
        .into_response()
}

fn app(state: Shared, allowed_hosts: Vec<String>) -> Router {
    let allowed_hosts = Arc::new(allowed_hosts);
    Router::new()
        .route(
            "/",
            get(|| async {
                asset(
                    include_str!("../web/index.html"),
                    "text/html; charset=utf-8",
                )
            }),
        )
        .route(
            "/index.html",
            get(|| async {
                asset(
                    include_str!("../web/index.html"),
                    "text/html; charset=utf-8",
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                asset(
                    include_str!("../web/app.js"),
                    "text/javascript; charset=utf-8",
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async { asset(include_str!("../web/style.css"), "text/css; charset=utf-8") }),
        )
        .route(
            "/vendor/prism.min.js",
            get(|| async {
                asset(
                    include_str!("../web/vendor/prism.min.js"),
                    "text/javascript; charset=utf-8",
                )
            }),
        )
        .route(
            "/vendor/prism.LICENSE",
            get(|| async {
                asset(
                    include_str!("../web/vendor/prism.LICENSE"),
                    "text/plain; charset=utf-8",
                )
            }),
        )
        .route(
            "/highlight.js",
            get(|| async {
                asset(
                    include_str!("../web/highlight.js"),
                    "text/javascript; charset=utf-8",
                )
            }),
        )
        .route(
            "/highlight.css",
            get(|| async {
                asset(
                    include_str!("../web/highlight.css"),
                    "text/css; charset=utf-8",
                )
            }),
        )
        .route("/api/config", get(config))
        .route("/api/repos", get(repos))
        .route("/api/review", get(review))
        .route("/api/diff", get(diff))
        .fallback(not_found)
        .method_not_allowed_fallback(wrong_method)
        .with_state(state)
        .layer(axum::middleware::from_fn(move |request, next| {
            let allowed_hosts = allowed_hosts.clone();
            async move { security::protect(request, next, &allowed_hosts).await }
        }))
}

async fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()
        .map_err(|error| format!("Cannot read current directory: {error}"))?;
    let state = repositories::Repositories::new(
        cli.repo.as_deref(),
        cli.base,
        cli.pr,
        cwd,
        std::env::var_os("HOME").map(PathBuf::from),
    )?;
    let app = app(Arc::new(state), cli.allow_host);
    let listener = tokio::net::TcpListener::bind(cli.bind)
        .await
        .map_err(|error| format!("Cannot bind {}: {error}", cli.bind))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("Cannot read listen address: {error}"))?;
    eprintln!("Review server: http://{address}");
    axum::serve(listener, app)
        .await
        .map_err(|error| format!("HTTP server failed: {error}"))
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("review: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;

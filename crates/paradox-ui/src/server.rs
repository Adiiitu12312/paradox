use anyhow::Result;
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State,
        ws::{Message, WebSocketUpgrade},
    },
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response, Sse, sse::KeepAlive},
    routing::{get, post},
};
use paradox_core::{repo::Repo, ui_actions, ui_git};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

#[derive(Clone, Default)]
pub struct UiOptions {
    pub assets_dir: Option<PathBuf>,
}
pub struct UiReady {
    pub address: SocketAddr,
    pub url: String,
}
struct Shared {
    repo: Repo,
    options: UiOptions,
    token: String,
    host: String,
    origin: String,
    tabs: AtomicUsize,
    generation: AtomicU64,
    shutdown: watch::Sender<bool>,
    operations: Arc<crate::operations::Operations>,
}
type App = Arc<Shared>;
fn failure(status: StatusCode, code: &str, error: impl ToString) -> Response {
    (
        status,
        Json(json!({"error":error.to_string(), "code":code, "commands":[]})),
    )
        .into_response()
}
async fn authenticate(State(app): State<App>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        != Some(app.host.as_str())
    {
        return failure(StatusCode::FORBIDDEN, "forbidden", "Invalid Host");
    }
    let origin = headers.get(header::ORIGIN);
    let requires_origin = request.method() != Method::GET && request.method() != Method::HEAD
        || request.uri().path() == "/api/session";
    if origin.is_some_and(|value| value.to_str().ok() != Some(app.origin.as_str()))
        || requires_origin && origin.is_none()
    {
        return failure(StatusCode::FORBIDDEN, "forbidden", "Invalid Origin");
    }
    if request.method() == Method::GET
        && request.uri().path() == "/"
        && let Some(query) = request.uri().query()
        && query == format!("token={}", app.token)
    {
        let mut response = Redirect::to("/").into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            format!(
                "paradox_session={}; Path=/; HttpOnly; SameSite=Strict",
                app.token
            )
            .parse()
            .expect("hex cookie"),
        );
        secure_headers(&mut response);
        return response;
    }
    let authorized = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .any(|cookie| cookie.trim().strip_prefix("paradox_session=") == Some(app.token.as_str()));
    if !authorized {
        return failure(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Open the session URL printed by paradox ui",
        );
    }
    let mut response = next.run(request).await;
    secure_headers(&mut response);
    response
}
fn secure_headers(response: &mut Response) {
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; worker-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, value.parse().expect("static security header"));
    }
}
async fn blocking<F>(operation: F) -> Response
where
    F: FnOnce() -> Result<Value> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => {
            if let Some(action) = error.downcast_ref::<ui_git::ActionError>() {
                let status = if matches!(action.code, "stale_diff" | "stale_plan" | "operation_busy") {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::UNPROCESSABLE_ENTITY
                };
                (status, Json(json!({"error":action.message,"code":action.code,"commands":action.commands}))).into_response()
            } else {
                failure(StatusCode::BAD_REQUEST, "invalid_request", error)
            }
        }
        Err(error) => failure(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", error),
    }
}
async fn status(State(app): State<App>) -> Response {
    blocking(move || ui_git::status(app.repo.clone())).await
}
#[derive(Deserialize)]
struct GraphQuery {
    cursor: Option<String>,
}
async fn graph(State(app): State<App>, Query(query): Query<GraphQuery>) -> Response {
    blocking(move || ui_git::graph(app.repo.clone(), query.cursor.as_deref())).await
}
async fn commit_detail(State(app): State<App>, Path(sha): Path<String>) -> Response {
    blocking(move || ui_git::commit_detail(app.repo.clone(), &sha)).await
}
#[derive(Deserialize)]
struct DiffQuery {
    path: String,
    view: String,
    commit: Option<String>,
}
async fn diff(State(app): State<App>, Query(query): Query<DiffQuery>) -> Response {
    blocking(move || {
        ui_git::diff(
            app.repo.clone(),
            &query.path,
            &query.view,
            query.commit.as_deref(),
        )
    })
    .await
}
async fn stage(State(app): State<App>, Json(request): Json<ui_git::StageRequest>) -> Response {
    let repo = app.repo.clone();
    blocking_edit(app, move || ui_git::stage(repo, request, false)).await
}
async fn unstage(State(app): State<App>, Json(request): Json<ui_git::StageRequest>) -> Response {
    let repo = app.repo.clone();
    blocking_edit(app, move || ui_git::stage(repo, request, true)).await
}
async fn commit(State(app): State<App>, Json(request): Json<ui_git::CommitRequest>) -> Response {
    let repo = app.repo.clone();
    blocking_edit(app, move || ui_git::commit(repo, request, &std::env::current_exe()?)).await
}
async fn blocking_edit<F>(app: App, operation: F) -> Response
where F: FnOnce() -> Result<Value> + Send + 'static {
    blocking(move || {
        let _guard = app.operations.mutation.try_lock().map_err(|_| ui_git::ActionError {
            code: "operation_busy", message: "Another Git operation is running".to_owned(), commands: vec![],
        })?;
        if app.operations.busy() {
            return Err(ui_git::ActionError { code: "operation_busy", message: "Another Git operation is running".to_owned(), commands: vec![] }.into());
        }
        operation()
    }).await
}
async fn actions_state(State(app): State<App>) -> Response {
    blocking(move || ui_actions::state(app.repo.clone())).await
}
#[derive(Deserialize)]
struct FileQuery { path: String, revision: Option<String>, cursor: Option<String> }
async fn blame(State(app): State<App>, Query(query): Query<FileQuery>) -> Response {
    blocking(move || ui_actions::blame(app.repo.clone(), &query.path, query.revision.as_deref())).await
}
async fn file_history(State(app): State<App>, Query(query): Query<FileQuery>) -> Response {
    blocking(move || ui_actions::history(app.repo.clone(), &query.path, query.cursor.as_deref())).await
}
async fn conflict(State(app): State<App>, Query(query): Query<FileQuery>) -> Response {
    blocking(move || ui_actions::conflict(app.repo.clone(), &query.path)).await
}
async fn resolve_conflict(State(app): State<App>, Json(request): Json<ui_actions::ResolveRequest>) -> Response {
    let repo = app.repo.clone();
    blocking_edit(app, move || ui_actions::resolve_conflict(repo, request)).await
}
async fn preview_action(State(app): State<App>, Json(action): Json<ui_actions::Action>) -> Response {
    blocking(move || {
        let plan = ui_actions::preview(app.repo.clone(), action)?;
        app.operations.remember(plan)
    }).await
}
#[derive(Deserialize)]
struct StartAction { action: ui_actions::Action, confirmation_token: Option<String> }
async fn start_action(State(app): State<App>, Json(request): Json<StartAction>) -> Response {
    match app.operations.start(app.repo.clone(), request.action, request.confirmation_token) {
        Ok(id) => Json(json!({"operation_id":id, "commands":[]})).into_response(),
        Err(error) => failure(StatusCode::CONFLICT, "operation_not_started", error),
    }
}
async fn progress(State(app): State<App>, Path(id): Path<String>, request: Request) -> Response {
    let Some(operation) = app.operations.get(&id) else { return failure(StatusCode::NOT_FOUND, "not_found", "Operation not found"); };
    let after = request.headers().get("last-event-id").and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u64>().ok()).unwrap_or(0);
    Sse::new(operation.stream(after)).keep_alive(KeepAlive::default()).into_response()
}
async fn cancel_action(State(app): State<App>, Path(id): Path<String>) -> Response {
    match app.operations.get(&id) {
        Some(operation) => { operation.cancel(); Json(json!({"cancelling":true,"commands":[]})).into_response() },
        None => failure(StatusCode::NOT_FOUND, "not_found", "Operation not found"),
    }
}
async fn asset(State(app): State<App>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    match tokio::task::spawn_blocking(move || crate::assets::get(&path, &app.options.assets_dir))
        .await
    {
        Ok(Ok(Some(asset))) => {
            ([(header::CONTENT_TYPE, asset.content_type)], asset.bytes).into_response()
        }
        Ok(Ok(None)) => failure(StatusCode::NOT_FOUND, "not_found", "Asset not found"),
        Ok(Err(error)) => failure(StatusCode::BAD_REQUEST, "invalid_request", error),
        Err(error) => failure(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", error),
    }
}
fn delayed_shutdown(app: App, generation: u64, delay: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if app.tabs.load(Ordering::SeqCst) == 0
            && app.generation.load(Ordering::SeqCst) == generation
        {
            let _ = app.shutdown.send(true);
        }
    });
}
async fn session(State(app): State<App>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |mut socket| async move {
        app.tabs.fetch_add(1, Ordering::SeqCst); app.generation.fetch_add(1, Ordering::SeqCst);
        let mut shutdown = app.shutdown.subscribe();
        loop {
            tokio::select! {
                event = socket.recv() => match event { Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break, _ => {} },
                _ = shutdown.changed() => { let _ = socket.send(Message::Close(None)).await; break; }
            }
        }
        if app.tabs.fetch_sub(1, Ordering::SeqCst) == 1 {
            let generation = app.generation.fetch_add(1, Ordering::SeqCst) + 1;
            delayed_shutdown(app, generation, Duration::from_secs(2));
        }
    })
}
pub async fn serve(
    repo: Repo,
    options: UiOptions,
    on_ready: impl FnOnce(UiReady) + Send + 'static,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let address = listener.local_addr()?;
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random)
        .map_err(|error| anyhow::anyhow!("session entropy unavailable: {error}"))?;
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let host = address.to_string();
    let origin = format!("http://{host}");
    let url = format!("{origin}/?token={token}");
    let (shutdown, mut receiver) = watch::channel(false);
    let app = Arc::new(Shared {
        repo,
        options,
        token,
        host,
        origin,
        tabs: AtomicUsize::new(0),
        generation: AtomicU64::new(0),
        shutdown,
        operations: Arc::new(crate::operations::Operations::default()),
    });
    let router = Router::new()
        .route("/api/status", get(status))
        .route("/api/graph", get(graph))
        .route("/api/commit/{sha}", get(commit_detail))
        .route("/api/diff", get(diff))
        .route("/api/stage", post(stage))
        .route("/api/unstage", post(unstage))
        .route("/api/commit", post(commit))
        .route("/api/session", get(session))
        .route("/api/actions/state", get(actions_state))
        .route("/api/actions/blame", get(blame))
        .route("/api/actions/history", get(file_history))
        .route("/api/actions/conflict", get(conflict))
        .route("/api/actions/resolve", post(resolve_conflict))
        .route("/api/actions/preview", post(preview_action))
        .route("/api/actions/start", post(start_action))
        .route("/api/operations/{id}/events", get(progress))
        .route("/api/operations/{id}/cancel", post(cancel_action))
        .fallback(asset)
        .layer(DefaultBodyLimit::max(2_097_152))
        .layer(middleware::from_fn_with_state(app.clone(), authenticate))
        .with_state(app.clone());
    let shutdown_sender = app.shutdown.clone();
    let operations = app.operations.clone();
    delayed_shutdown(app, 0, Duration::from_secs(60));
    on_ready(UiReady { address, url });
    axum::serve(listener, router).with_graceful_shutdown(async move {
        tokio::select! { _ = tokio::signal::ctrl_c() => { let _ = shutdown_sender.send(true); }, _ = async { while !*receiver.borrow_and_update() { if receiver.changed().await.is_err() { break; } } } => {} }
        operations.shutdown().await;
    }).await?;
    Ok(())
}

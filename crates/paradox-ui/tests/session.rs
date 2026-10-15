use paradox_core::repo::Repo;
use paradox_ui::{UiOptions, UiReady};
use reqwest::{Client, StatusCode};
use std::{ffi::OsString, process::Command, time::Duration};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};

struct Session {
    _directory: tempfile::TempDir,
    ready: UiReady,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    client: Client,
    cookie: String,
}
impl Session {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        assert!(status.success());
        let repo = Repo::discover(&[
            OsString::from("-C"),
            directory.path().as_os_str().to_owned(),
        ])
        .unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(paradox_ui::serve(
            repo,
            UiOptions::default(),
            move |ready| {
                let _ = sender.send(ready);
            },
        ));
        let ready = tokio::time::timeout(Duration::from_secs(10), receiver)
            .await
            .unwrap()
            .unwrap();
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let bootstrap = client.get(&ready.url).send().await.unwrap();
        assert_eq!(bootstrap.status(), StatusCode::SEE_OTHER);
        assert_eq!(bootstrap.headers()["location"], "/");
        let set_cookie = bootstrap.headers()["set-cookie"].to_str().unwrap();
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        let cookie = set_cookie.split(';').next().unwrap().to_owned();
        Self {
            _directory: directory,
            ready,
            task,
            client,
            cookie,
        }
    }
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.ready.address)
    }
    fn git(&self, args: &[&str]) {
        let output = Command::new("git").args(args)
            .current_dir(self._directory.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self._directory.path().join("no-global"))
            .env("GIT_AUTHOR_NAME", "Paradox Test").env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Paradox Test").env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
    fn commit_fixture(&self) {
        self.git(&["config", "commit.gpgsign", "false"]);
        self.git(&["config", "core.hooksPath", ".git/hooks"]);
        std::fs::write(self._directory.path().join("file.txt"), "base\n").unwrap();
        self.git(&["add", "--", "file.txt"]);
        self.git(&["commit", "-qm", "fixture"]);
    }
    async fn post(&self, path: &str, body: serde_json::Value) -> reqwest::Response {
        self.client.post(self.url(path)).header("cookie", &self.cookie)
            .header("origin", format!("http://{}", self.ready.address))
            .json(&body).send().await.unwrap()
    }
    async fn websocket(
        &self,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let mut request = format!("ws://{}/api/session", self.ready.address)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("cookie", HeaderValue::from_str(&self.cookie).unwrap());
        request.headers_mut().insert(
            "origin",
            HeaderValue::from_str(&format!("http://{}", self.ready.address)).unwrap(),
        );
        tokio_tungstenite::connect_async(request).await.unwrap().0
    }
}

#[tokio::test]
async fn every_route_requires_cookie_host_and_mutation_origin() {
    let session = Session::new().await;
    for path in [
        "/",
        "/api/status",
        "/api/graph",
        "/assets/app.js",
        "/api/session",
        "/api/actions/state",
        "/api/actions/history?path=a.txt",
        "/api/actions/blame?path=a.txt",
        "/api/actions/conflict?path=a.txt",
        "/api/operations/missing/events",
    ] {
        let request = session.client.get(session.url(path));
        let request = if path == "/api/session" {
            request.header("origin", format!("http://{}", session.ready.address))
        } else {
            request
        };
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    assert_eq!(
        session
            .client
            .get(session.url("/api/status"))
            .header("cookie", &session.cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    for (host, origin) in [
        (Some("evil.example"), None),
        (None, Some("https://evil.example")),
    ] {
        let mut request = session
            .client
            .get(session.url("/api/status"))
            .header("cookie", &session.cookie);
        if let Some(host) = host {
            request = request.header("host", host);
        }
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        session
            .client
            .post(session.url("/api/stage"))
            .header("cookie", &session.cookie)
            .json(&serde_json::json!({"mode":"all"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut socket = session.websocket().await;
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn last_tab_exit_allows_reload_and_leaves_no_server() {
    let session = Session::new().await;
    let mut first = session.websocket().await;
    let mut second = session.websocket().await;
    first.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(
        !session.task.is_finished(),
        "one remaining tab keeps the session alive"
    );
    second.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut reload = session.websocket().await;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(
        !session.task.is_finished(),
        "reload supersedes the earlier shutdown deadline"
    );
    reload.close(None).await.unwrap();
    let address = session.ready.address;
    tokio::time::timeout(Duration::from_secs(10), session.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn typed_action_streams_native_result_and_confirmation_cannot_be_reused() {
    let session = Session::new().await;
    session.commit_fixture();
    let mut socket = session.websocket().await;
    let action = serde_json::json!({"action":"branch_create","name":"feature"});
    let preview = session.post("/api/actions/preview", action.clone()).await;
    assert_eq!(preview.status(), StatusCode::OK);
    let plan: serde_json::Value = preview.json().await.unwrap();
    let request = serde_json::json!({"action":action,"confirmation_token":plan["confirmation_token"]});
    let started = session.post("/api/actions/start", request.clone()).await;
    assert_eq!(started.status(), StatusCode::OK);
    let started: serde_json::Value = started.json().await.unwrap();
    let id = started["operation_id"].as_str().unwrap();
    let transcript = session.client.get(session.url(&format!("/api/operations/{id}/events")))
        .header("cookie", &session.cookie).send().await.unwrap().text().await.unwrap();
    assert!(transcript.contains("\"type\":\"command\""));
    assert!(transcript.contains("\"type\":\"completed\""));
    assert!(transcript.contains("\"ok\":true"), "{transcript}");
    session.git(&["rev-parse", "--verify", "refs/heads/feature"]);
    assert_eq!(session.post("/api/actions/start", request).await.status(), StatusCode::CONFLICT);
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.task).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn closing_last_tab_cancels_and_reaps_a_git_hook_operation() {
    let session = Session::new().await;
    session.commit_fixture();
    session.git(&["branch", "feature"]);
    let hook = session._directory.path().join(".git/hooks/post-checkout");
    std::fs::write(&hook, "#!/bin/sh\nprintf 'hook is running\\n'\nsleep 60\n").unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut socket = session.websocket().await;
    let action = serde_json::json!({"action":"branch_switch","name":"feature"});
    let preview = session.post("/api/actions/preview", action.clone()).await;
    assert_eq!(preview.status(), StatusCode::OK);
    let plan: serde_json::Value = preview.json().await.unwrap();
    let started = session.post("/api/actions/start", serde_json::json!({"action":action,"confirmation_token":plan["confirmation_token"]})).await;
    assert_eq!(started.status(), StatusCode::OK);
    let started: serde_json::Value = started.json().await.unwrap();
    let id = started["operation_id"].as_str().unwrap();
    let mut response = session.client.get(session.url(&format!("/api/operations/{id}/events")))
        .header("cookie", &session.cookie).send().await.unwrap();
    let mut transcript = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !transcript.contains("hook is running") {
            let chunk = response.chunk().await.unwrap().expect("operation exited before hook");
            transcript.push_str(&String::from_utf8_lossy(&chunk));
        }
    }).await.unwrap();
    socket.close(None).await.unwrap();
    // The SSE response stays open: shutdown must end the job before draining connections.
    tokio::time::timeout(Duration::from_secs(10), async { while response.chunk().await.unwrap().is_some() {} }).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.task).await.unwrap().unwrap().unwrap();
}

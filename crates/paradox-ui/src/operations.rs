//! Session-owned Git jobs and bounded, reconnectable progress transcripts.
use anyhow::{Result, bail, ensure};
use axum::response::sse::Event;
use paradox_core::{repo::Repo, ui_actions::{self, Action, Cancellation, Plan}};
use serde_json::{Value, json};
use std::{collections::{HashMap, VecDeque}, convert::Infallible, sync::{Arc, Mutex}, time::{Duration, Instant}};
use tokio::sync::watch;

const MAX_EVENTS: usize = 256;
const MAX_BYTES: usize = 2_097_152;

fn identifier() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("operation entropy unavailable: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
struct Transcript { events: VecDeque<(u64, String)>, next: u64, bytes: usize, done: bool }
pub(crate) struct Operation {
    cancellation: Cancellation,
    transcript: Mutex<Transcript>,
    revision: watch::Sender<u64>,
}
impl Operation {
    fn new() -> Self {
        let (revision, _) = watch::channel(0);
        Self { cancellation: Cancellation::new(), transcript: Mutex::new(Transcript { events: VecDeque::new(), next: 1, bytes: 0, done: false }), revision }
    }
    fn emit(&self, value: Value, done: bool) {
        let mut value = value;
        if let Some(text) = value.get("text").and_then(Value::as_str).map(str::to_owned) {
            if text.len() > 8192 {
                let mut end = 8192;
                while !text.is_char_boundary(end) { end -= 1; }
                value["text"] = json!(&text[..end]);
                value["truncated"] = json!(true);
            }
        }
        let encoded = value.to_string();
        let mut transcript = self.transcript.lock().unwrap_or_else(|error| error.into_inner());
        let id = transcript.next;
        transcript.next += 1;
        transcript.bytes += encoded.len();
        transcript.events.push_back((id, encoded));
        while transcript.events.len() > MAX_EVENTS || transcript.bytes > MAX_BYTES && transcript.events.len() > 1 {
            if let Some((_, item)) = transcript.events.pop_front() { transcript.bytes -= item.len(); }
        }
        transcript.done |= done;
        self.revision.send_replace(id);
    }
    fn done(&self) -> bool { self.transcript.lock().unwrap_or_else(|error| error.into_inner()).done }
    pub(crate) fn cancel(&self) { self.cancellation.cancel(); }
    pub(crate) fn stream(self: Arc<Self>, after: u64) -> impl futures_util::Stream<Item = Result<Event, Infallible>> + Send + 'static {
        let revision = self.revision.subscribe();
        futures_util::stream::unfold((self, revision, after), |(operation, mut revision, mut after)| async move {
            loop {
                let (item, done) = {
                    let transcript = operation.transcript.lock().unwrap_or_else(|error| error.into_inner());
                    if let Some((first, _)) = transcript.events.front() {
                        if after.saturating_add(1) < *first {
                            let skipped = *first - after - 1;
                            after = *first - 1;
                            (Some((after, json!({"type":"gap", "text":format!("{skipped} older progress events expired; the terminal result is retained.")}).to_string())), false)
                        } else { (transcript.events.iter().find(|(id, _)| *id > after).cloned(), transcript.done) }
                    } else { (None, transcript.done) }
                };
                if let Some((id, data)) = item {
                    return Some((Ok(Event::default().id(id.to_string()).data(data)), (operation, revision, id)));
                }
                if done || revision.changed().await.is_err() { return None; }
            }
        })
    }
}
struct Confirmation { plan: Plan, created: Instant }
#[derive(Default)]
pub(crate) struct Operations {
    jobs: Mutex<HashMap<String, Arc<Operation>>>,
    plans: Mutex<HashMap<String, Confirmation>>,
    pub(crate) mutation: Mutex<()>,
}
impl Operations {
    pub(crate) fn remember(&self, plan: Plan) -> Result<Value> {
        let token = identifier()?;
        let mut value = serde_json::to_value(&plan)?;
        let mut plans = self.plans.lock().unwrap_or_else(|error| error.into_inner());
        plans.retain(|_, plan| plan.created.elapsed() < Duration::from_secs(600));
        ensure!(plans.len() < 32, "Too many pending confirmations; refresh and try again shortly");
        plans.insert(token.clone(), Confirmation { plan, created: Instant::now() });
        value["confirmation_token"] = json!(token);
        Ok(value)
    }
    pub(crate) fn get(&self, id: &str) -> Option<Arc<Operation>> {
        self.jobs.lock().unwrap_or_else(|error| error.into_inner()).get(id).cloned()
    }
    pub(crate) fn busy(&self) -> bool {
        self.jobs.lock().unwrap_or_else(|error| error.into_inner()).values().any(|operation| !operation.done())
    }
    pub(crate) fn start(self: &Arc<Self>, repo: Repo, action: Action, confirmation: Option<String>) -> Result<String> {
        let plan = match confirmation {
            Some(token) => {
                let confirmation = self.plans.lock().unwrap_or_else(|error| error.into_inner()).remove(&token);
                let Some(confirmation) = confirmation else { bail!("Confirmation expired; preview the action again"); };
                ensure!(confirmation.created.elapsed() < Duration::from_secs(600), "Confirmation expired; preview the action again");
                ensure!(serde_json::to_value(&confirmation.plan.action)? == serde_json::to_value(&action)?, "Confirmation does not match this action");
                Some(confirmation.plan)
            }, None => None,
        };
        let id = identifier()?;
        let operation = Arc::new(Operation::new());
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(|error| error.into_inner());
            ensure!(!jobs.values().any(|job| !job.done()), "Another Git operation is running");
            if jobs.len() >= 32 { jobs.retain(|_, job| !job.done()); }
            jobs.insert(id.clone(), operation.clone());
        }
        let operations = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = operations.mutation.lock().unwrap_or_else(|error| error.into_inner());
            operation.emit(json!({"type":"start"}), false);
            let execution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ui_actions::execute(repo, action, plan.as_ref(), operation.cancellation.clone(), |event| {
                match serde_json::to_value(event) {
                    Ok(value) => operation.emit(value, false),
                    Err(error) => operation.emit(json!({"type":"stderr", "text":error.to_string()}), false),
                }
            })));
            let result = execution.unwrap_or_else(|_| Err(anyhow::anyhow!("Git operation worker stopped unexpectedly")));
            match result {
                Ok(value) => operation.emit(json!({"type":"completed", "ok":true, "result":value}), true),
                Err(error) => {
                    let action = error.downcast_ref::<paradox_core::ui_git::ActionError>();
                    operation.emit(json!({"type":"completed", "ok":false, "error":error.to_string(), "code":action.map(|error| error.code).unwrap_or("operation_failed"), "commands":action.map(|error| &error.commands)}), true);
                }
            }
        });
        Ok(id)
    }
    pub(crate) async fn shutdown(&self) {
        let jobs: Vec<_> = self.jobs.lock().unwrap_or_else(|error| error.into_inner()).values().cloned().collect();
        for job in &jobs { job.cancel(); }
        // The core runner terminates and reaps its owned process tree on cancel.
        for job in jobs {
            let mut revision = job.revision.subscribe();
            while !job.done() { if revision.changed().await.is_err() { break; } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn reconnect_reports_evicted_progress_and_keeps_the_terminal_result() {
        let operation = Arc::new(Operation::new());
        for _ in 0..300 { operation.emit(json!({"type":"stdout","text":"output"}), false); }
        operation.emit(json!({"type":"completed","ok":true}), true);
        let stream = operation.clone().stream(0);
        futures_util::pin_mut!(stream);
        let mut count = 0;
        while stream.next().await.is_some() { count += 1; }
        assert_eq!(count, MAX_EVENTS + 1, "one gap event plus the retained bounded transcript");
        let transcript = operation.transcript.lock().unwrap();
        assert!(transcript.bytes <= MAX_BYTES);
        assert!(transcript.events.back().unwrap().1.contains("completed"));
    }

    #[tokio::test]
    async fn reconnect_after_terminal_id_finishes_without_waiting_or_overflowing() {
        let operation = Arc::new(Operation::new());
        operation.emit(json!({"type":"completed","ok":true}), true);
        let stream = operation.stream(u64::MAX);
        futures_util::pin_mut!(stream);
        assert!(stream.next().await.is_none());
    }
}

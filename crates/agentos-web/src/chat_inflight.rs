//! In-flight chat inference registry.
//!
//! Each running `chat_infer_streaming` task pushes `ChatStreamEvent`s into a per-session
//! buffer kept in memory. SSE subscribers attach to the buffer, replay anything that has
//! already been emitted, then block on a `Notify` for new events until the task marks the
//! entry `done`. This lets a browser refresh reconnect to an inference that the server
//! is still streaming, instead of orphaning the user message.

use agentos_kernel::kernel::ChatStreamEvent;
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;

/// Grace period after `mark_done` before the entry is evicted from the map. Lets a late
/// refresh still replay the final events — crucial for error messages that are not
/// persisted to the chat store.
const POST_DONE_RETENTION: std::time::Duration = std::time::Duration::from_secs(60);
/// Safety cap for buffered events per in-flight session.
const MAX_BUFFERED_EVENTS: usize = 10_000;
/// When the buffer reaches this threshold, coalesce old TextChunk events into a
/// single prefix string to keep the vec small without losing streamed text.
const COALESCE_THRESHOLD: usize = 8_000;

pub struct InFlightInference {
    inner: Mutex<InFlightInner>,
    notify: Notify,
    task_handle: StdMutex<Option<JoinHandle<()>>>,
    /// Sync-safe done flag so `try_start` can check without async.
    done_flag: AtomicBool,
}

struct InFlightInner {
    events: Vec<ChatStreamEvent>,
    /// Text from old TextChunk events that were coalesced to keep the vec bounded.
    /// When a new subscriber attaches, this is replayed as a single synthetic TextChunk
    /// before the remaining events.
    coalesced_text_prefix: String,
    done: bool,
}

impl InFlightInner {
    /// Merge consecutive TextChunk events from the front of the vec into
    /// `coalesced_text_prefix`, reducing the vec by up to `target` entries.
    fn coalesce_old_text(&mut self, target: usize) {
        let mut merged = String::new();
        let mut drained = 0;
        self.events.retain(|e| {
            if drained >= target {
                return true;
            }
            if let ChatStreamEvent::TextChunk { ref text } = e {
                merged.push_str(text);
                drained += 1;
                false
            } else {
                true
            }
        });
        self.coalesced_text_prefix.push_str(&merged);
    }
}

impl InFlightInference {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(InFlightInner {
                events: Vec::new(),
                coalesced_text_prefix: String::new(),
                done: false,
            }),
            notify: Notify::new(),
            task_handle: StdMutex::new(None),
            done_flag: AtomicBool::new(false),
        })
    }

    /// Sync-safe check: has this inference finished?
    pub fn is_done(&self) -> bool {
        self.done_flag.load(Ordering::Acquire)
    }

    pub fn set_task_handle(&self, handle: JoinHandle<()>) {
        if self.is_done() {
            handle.abort();
            return;
        }
        let mut slot = self.task_handle.lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(handle);
    }

    pub async fn cancel(&self, message: impl Into<String>) -> bool {
        if self.done_flag.swap(true, Ordering::AcqRel) {
            return false;
        }

        if let Some(handle) = self
            .task_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            handle.abort();
        }

        let mut inner = self.inner.lock().await;
        inner.events.push(ChatStreamEvent::Done {
            answer: message.into(),
            tool_calls: Vec::new(),
            iterations: 0,
            tokens_used: 0,
            cost_usd: 0.0,
        });
        inner.done = true;
        drop(inner);
        self.notify.notify_waiters();
        true
    }

    pub async fn push(&self, event: ChatStreamEvent) {
        if self.is_done() {
            return;
        }
        let mut inner = self.inner.lock().await;
        if inner.done {
            return;
        }
        if inner.events.len() >= COALESCE_THRESHOLD {
            inner.coalesce_old_text(2_000);
        }
        inner.events.push(event);
        if inner.events.len() > MAX_BUFFERED_EVENTS {
            let excess = inner.events.len() - MAX_BUFFERED_EVENTS;
            inner.events.drain(0..excess);
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    pub async fn mark_done(&self) {
        self.done_flag.store(true, Ordering::Release);
        let mut inner = self.inner.lock().await;
        inner.done = true;
        drop(inner);
        self.notify.notify_waiters();
    }

    async fn read_from(&self, cursor: usize) -> (Vec<ChatStreamEvent>, usize, bool) {
        let inner = self.inner.lock().await;
        let start = if cursor > inner.events.len() {
            0
        } else {
            cursor
        };
        let mut new: Vec<ChatStreamEvent> = Vec::new();
        // On initial replay (cursor == 0), prepend the coalesced text prefix so the
        // subscriber gets the full streamed text even though the individual TextChunk
        // events were merged.
        if cursor == 0 && !inner.coalesced_text_prefix.is_empty() {
            new.push(ChatStreamEvent::TextChunk {
                text: inner.coalesced_text_prefix.clone(),
            });
        }
        if start < inner.events.len() {
            new.extend_from_slice(&inner.events[start..]);
        }
        (new, inner.events.len(), inner.done)
    }

    /// Drive a subscriber loop: replay buffered events first, then stream live events
    /// into `tx` until the inference completes or the client disconnects.
    ///
    /// The `Notify::notified()` future is enabled BEFORE the second state read to close
    /// the race where a producer pushes between `read_from` and the wait. Any event the
    /// subscriber missed due to a wakeup landing slightly early is caught by that
    /// double-check. Wakeups that arrive after cursor catch-up are consumed by
    /// `notified.await` on the next pass.
    pub async fn subscribe_events(&self, tx: mpsc::Sender<ChatStreamEvent>) {
        let mut cursor = 0usize;
        loop {
            let (new_events, new_cursor, done) = self.read_from(cursor).await;
            cursor = new_cursor;
            for ev in new_events {
                if tx.send(ev).await.is_err() {
                    return;
                }
            }
            if done {
                return;
            }

            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let (new_events, new_cursor, done) = self.read_from(cursor).await;
            cursor = new_cursor;
            for ev in new_events {
                if tx.send(ev).await.is_err() {
                    return;
                }
            }
            if done {
                return;
            }

            tokio::select! {
                _ = notified => {}
                _ = tx.closed() => return,
            }
        }
    }
}

#[derive(Default)]
pub struct InFlightChat {
    sessions: DashMap<String, Arc<InFlightInference>>,
}

impl InFlightChat {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `Some(handle)` if a new entry was created; `None` if the session already
    /// has a **running** inference. Callers should treat `None` as a 409 conflict and
    /// refuse the request rather than orphaning a user message or kicking off two LLM
    /// calls.
    ///
    /// If a previous inference has completed (done) but its retention window has not yet
    /// elapsed, the old entry is replaced so the next message can proceed immediately.
    pub fn try_start(&self, session_id: &str) -> Option<Arc<InFlightInference>> {
        let entry = InFlightInference::new();
        match self.sessions.entry(session_id.to_string()) {
            dashmap::mapref::entry::Entry::Occupied(mut o) => {
                if o.get().is_done() {
                    // Previous inference finished — replace the stale entry.
                    o.insert(Arc::clone(&entry));
                    Some(entry)
                } else {
                    None
                }
            }
            dashmap::mapref::entry::Entry::Vacant(v) => {
                v.insert(Arc::clone(&entry));
                Some(entry)
            }
        }
    }

    pub fn get(&self, session_id: &str) -> Option<Arc<InFlightInference>> {
        self.sessions.get(session_id).map(|e| Arc::clone(e.value()))
    }

    /// Release a reserved slot without running an inference. Used when persistence of
    /// the user message fails after `try_start` has already succeeded.
    pub fn abandon(&self, session_id: &str) {
        self.sessions.remove(session_id);
    }

    /// Schedule removal of the entry after the retention window so late subscribers can
    /// still replay the last events before the slot is freed.
    pub fn schedule_cleanup(self: &Arc<Self>, session_id: String) {
        let map = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(POST_DONE_RETENTION).await;
            map.sessions.remove(&session_id);
        });
    }
}

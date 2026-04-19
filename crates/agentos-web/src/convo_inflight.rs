//! In-flight multi-agent conversation registry.
//!
//! Each running conversation orchestrator pushes `ConvoStreamEvent`s into a per-conversation
//! buffer. SSE subscribers attach, replay buffered events from cursor 0, then block until
//! the orchestrator marks the entry done. A browser refresh reconnects and replays everything.
//!
//! ## Sequence number design
//!
//! The replay buffer uses monotonic sequence numbers to survive truncation. Every event is
//! assigned a global sequence number; `base_seq` tracks the sequence number of `events[0]`.
//! A subscriber's cursor is the last sequence number it has *sent* (starts at `u64::MAX` to
//! mean "nothing sent yet"). After a drain, `base_seq` advances; subscribers whose cursor
//! falls below `base_seq` receive a truncation-notice synthetic event and then resume from
//! `base_seq`. This prevents the index-shift bug that would otherwise cause events to be
//! silently skipped or replayed after a buffer truncation.

use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

const POST_DONE_RETENTION: std::time::Duration = std::time::Duration::from_secs(120);
/// Maximum number of events kept per conversation. When exceeded, oldest events are
/// dropped and a truncation notice is issued to subscribers on their next read.
const MAX_BUFFERED_EVENTS: usize = 2_000;

/// Sentinel cursor meaning "subscriber has not seen any events yet."
const CURSOR_INIT: u64 = u64::MAX;

/// Events emitted during a live multi-agent conversation.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum ConvoStreamEvent {
    /// An agent is starting their turn.
    TurnStart { agent: String, turn: u32 },
    /// LLM is reasoning for this agent's turn.
    Thinking {
        agent: String,
        turn: u32,
        iteration: u32,
    },
    /// Incremental text from the LLM.
    TextChunk {
        agent: String,
        turn: u32,
        text: String,
    },
    /// A tool call started.
    ToolStart {
        agent: String,
        turn: u32,
        tool_name: String,
        iteration: u32,
    },
    /// A tool call completed.
    ToolResult {
        agent: String,
        turn: u32,
        tool_name: String,
        result_preview: String,
        duration_ms: u64,
        success: bool,
    },
    /// An agent finished their turn with a complete response.
    TurnEnd {
        agent: String,
        turn: u32,
        answer: String,
    },
    /// The whole conversation finished.
    ConversationDone { total_turns: u32 },
    /// An error occurred (agent offline, inference failure, buffer overflow, etc.).
    Error { message: String },
}

struct InFlightInner {
    events: Vec<ConvoStreamEvent>,
    /// Absolute sequence number of `events[0]`. Advances on every drain.
    base_seq: u64,
    /// Sequence number to assign to the *next* pushed event.
    next_seq: u64,
    done: bool,
    /// Whether a truncation has ever occurred (so new subscribers know history may be lost).
    truncation_occurred: bool,
}

pub struct InFlightConvo {
    inner: std::sync::Mutex<InFlightInner>,
    notify: Notify,
    done_flag: AtomicBool,
}

impl InFlightConvo {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(InFlightInner {
                events: Vec::new(),
                base_seq: 0,
                next_seq: 0,
                done: false,
                truncation_occurred: false,
            }),
            notify: Notify::new(),
            done_flag: AtomicBool::new(false),
        })
    }

    pub fn is_done(&self) -> bool {
        self.done_flag.load(Ordering::Acquire)
    }

    /// Push an event into the replay buffer (sync — no await needed).
    pub fn push(&self, event: ConvoStreamEvent) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.events.push(event);
            inner.next_seq += 1;
            if inner.events.len() > MAX_BUFFERED_EVENTS {
                let excess = inner.events.len() - MAX_BUFFERED_EVENTS;
                inner.events.drain(0..excess);
                inner.base_seq += excess as u64;
                inner.truncation_occurred = true;
            }
        }
        self.notify.notify_waiters();
    }

    /// Mark the conversation as finished (sync).
    pub fn mark_done(&self) {
        self.done_flag.store(true, Ordering::Release);
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.done = true;
        }
        self.notify.notify_waiters();
    }

    /// Read events with sequence number > `cursor`.
    ///
    /// `cursor` is the sequence number of the last event the caller has sent.
    /// Use `CURSOR_INIT` (`u64::MAX`) to request all buffered events from the start.
    ///
    /// Returns `(events_to_send, new_cursor, done)`.
    fn read_from(&self, cursor: u64) -> (Vec<ConvoStreamEvent>, u64, bool) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        let mut result = Vec::new();

        // Determine the effective start index in `inner.events`.
        let start_seq = if cursor == CURSOR_INIT {
            // New subscriber: start from the beginning of the buffer.
            inner.base_seq
        } else {
            cursor + 1
        };

        // If the desired start falls before base_seq, the subscriber missed events.
        if inner.truncation_occurred && start_seq < inner.base_seq {
            result.push(ConvoStreamEvent::Error {
                message: "Earlier events were dropped due to buffer overflow — \
                     reload the page to see persisted turns."
                    .into(),
            });
        }

        let effective_start = start_seq.max(inner.base_seq);
        let idx = (effective_start - inner.base_seq) as usize;
        if idx < inner.events.len() {
            result.extend_from_slice(&inner.events[idx..]);
        }

        // New cursor = sequence number of the last event we are returning (if any),
        // otherwise unchanged. Use `next_seq - 1` as the "high water mark" when we
        // sent everything up to the end of the buffer.
        let new_cursor = if inner.next_seq == 0 {
            CURSOR_INIT
        } else if cursor == CURSOR_INIT || start_seq <= inner.base_seq {
            // We started from the beginning — advance cursor to end of buffer.
            if inner.next_seq > 0 {
                inner.next_seq - 1
            } else {
                CURSOR_INIT
            }
        } else {
            // We resumed — advance to wherever we read up to.
            let sent_through = effective_start + result.len() as u64;
            if sent_through > 0 {
                sent_through - 1
            } else {
                cursor
            }
        };

        (result, new_cursor, inner.done)
    }

    pub async fn subscribe_events(&self, tx: mpsc::Sender<ConvoStreamEvent>) {
        let mut cursor = CURSOR_INIT;
        loop {
            let (events, new_cursor, done) = self.read_from(cursor);
            cursor = new_cursor;
            for ev in events {
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

            // Re-read before blocking so events pushed between the first read and
            // pinning the notification are not missed.
            let (events, new_cursor, done) = self.read_from(cursor);
            cursor = new_cursor;
            for ev in events {
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
pub struct InFlightConvos {
    convos: DashMap<String, Arc<InFlightConvo>>,
}

impl InFlightConvos {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `Some(handle)` if a new entry was created; `None` if already running.
    pub fn try_start(&self, convo_id: &str) -> Option<Arc<InFlightConvo>> {
        let entry = InFlightConvo::new();
        match self.convos.entry(convo_id.to_string()) {
            dashmap::mapref::entry::Entry::Occupied(mut o) => {
                if o.get().is_done() {
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

    pub fn get(&self, convo_id: &str) -> Option<Arc<InFlightConvo>> {
        self.convos.get(convo_id).map(|e| Arc::clone(e.value()))
    }

    /// Number of conversations whose orchestrators are still running.
    pub fn active_count(&self) -> usize {
        self.convos.iter().filter(|r| !r.value().is_done()).count()
    }

    pub fn schedule_cleanup(self: &Arc<Self>, convo_id: String) {
        let map = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(POST_DONE_RETENTION).await;
            map.convos.remove(&convo_id);
        });
    }
}

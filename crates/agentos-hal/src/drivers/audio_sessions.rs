//! Supervised playback sessions for the PipeWire audio driver.
//!
//! Playback used to run `pw-play` to completion *inside* the tool call, which
//! parked the calling agent's turn for the length of the track and left no
//! handle to pause or stop it — the child was owned by the run-to-completion
//! helper and dropped when it returned.
//!
//! Here a playback is a **session**: the tool call spawns the player, registers
//! it, and returns a `playback_id` immediately. One supervisor task per session
//! owns the [`Player`] and is the only thing that ever touches it.
//!
//! ## Why an actor and not a PID map
//!
//! Signalling a process by a PID read out of a map races the reap: the moment
//! `wait()` returns, the kernel may hand that PID to an unrelated process, and
//! a `SIGSTOP` meant for a finished track suspends a stranger.
//!
//! The supervisor holds ONE `wait()` future for the life of the player and
//! polls it by `&mut` reference, so a command winning the `select!` race
//! suspends that future rather than dropping it. The child is therefore never
//! reaped except on the one path that immediately stops signalling it.
//!
//! That structural guarantee is what matters; [`PidSignaller`] additionally
//! refuses to deliver a signal once the child has been reaped, so a future
//! refactor that reintroduces the drop degrades to a no-op instead of to
//! signalling a recycled PID.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agentos_types::AgentOSError;
use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::time::Instant;

/// Ceiling on simultaneously-playing sessions for one agent.
///
/// Every live session is audible at once, so an agent that loops on `playback`
/// without stopping anything is a pile-up, not a queue.
pub(crate) const MAX_PLAYBACKS_PER_AGENT: usize = 4;

/// Ceiling across every agent on the host.
///
/// Separate from the per-agent cap on purpose: a single global limit lets one
/// agent's four parked sessions lock every other agent out of audio entirely,
/// with no operator surface to clear them.
pub(crate) const MAX_PLAYBACKS_TOTAL: usize = 8;

/// How long a finished/stopped/failed session stays readable before pruning.
///
/// Playback is fire-and-forget, so a failure has no synchronous path back to
/// the agent — the record *is* the error report, and it has to outlive the turn
/// that started it.
const TERMINAL_RETENTION: Duration = Duration::from_secs(600);

/// Grace period for a player to exit after `SIGINT` before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// Longest a session may sit paused before it is stopped outright.
///
/// A paused session has its duration cap disabled, is never terminal and so is
/// never pruned, and holds a real `pw-play` in state `T` on a PipeWire stream.
/// Without this an agent that pauses and wanders off wedges a slot until the
/// kernel restarts.
const MAX_PAUSE: Duration = Duration::from_secs(900);

/// How long a lifecycle call waits for the supervisor to accept and confirm a
/// command. Comfortably past the two `STOP_GRACE` windows a stubborn player can
/// cost, so only a wedged supervisor ever hits it.
const COMMAND_ACK_TIMEOUT: Duration = Duration::from_secs(6);

/// Ceiling on the ffmpeg transcode. The supervisor is not in its `select!`
/// while transcoding, so an ffmpeg that never returns is a session that can
/// never be stopped.
const TRANSCODE_TIMEOUT: Duration = Duration::from_secs(120);

/// Cap on captured player stderr. Only ever read to classify a failure, and an
/// unbounded pipe drain is unbounded kernel RSS.
const MAX_STDERR_BYTES: u64 = 64 * 1024;

/// libsndfile reports an undecodable container as "Format not recognised".
/// (British spelling upstream; both are matched because the message has
/// changed spelling between builds.)
fn is_unsupported_format(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("format not recognised") || text.contains("format not recognized")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlayerSignal {
    Pause,
    Resume,
    Interrupt,
    Kill,
}

/// A lifecycle command from a tool call to a session's supervisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlayerCommand {
    Pause,
    Resume,
    Stop,
}

impl PlayerCommand {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Stop => "stop",
        }
    }
}

/// A command plus the ack the supervisor fires once it has applied it.
///
/// The ack carries the *outcome*, not just completion: a `SIGSTOP` that fails
/// must not be reported to the agent as a successful pause.
type Request = (PlayerCommand, oneshot::Sender<Result<(), String>>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlayerExit {
    pub code: i32,
    pub stderr: String,
}

/// Signal delivery, split out of [`Player`] so the supervisor can hold it
/// across a `select!` that mutably borrows the player for `wait()`.
pub(crate) trait PlayerSignaller: Send + Sync {
    fn signal(&self, signal: PlayerSignal) -> Result<(), AgentOSError>;
}

/// A spawned audio player process.
#[async_trait]
pub(crate) trait Player: Send {
    /// Cheap handle for signalling. Never reaps, never blocks.
    fn signaller(&self) -> Arc<dyn PlayerSignaller>;

    /// Wait for exit, draining stderr. Called exactly once per player: the
    /// supervisor keeps the returned future alive across its whole `select!`
    /// loop rather than re-creating it, so this need not be cancel-safe.
    async fn wait(&mut self) -> PlayerExit;
}

#[async_trait]
pub(crate) trait PlayerSpawner: Send + Sync {
    /// Spawn a detached player. Errors here (missing binary, EACCES) surface
    /// synchronously to the agent; everything after is supervised.
    async fn spawn(&self, program: &str, args: &[String]) -> Result<Box<dyn Player>, AgentOSError>;
}

struct PidSignaller {
    pid: i32,
    /// Set the instant the child is reaped. Signalling a reaped PID is how a
    /// stranger process gets suspended, so the gate is checked on every send.
    reaped: Arc<AtomicBool>,
}

impl PlayerSignaller for PidSignaller {
    fn signal(&self, signal: PlayerSignal) -> Result<(), AgentOSError> {
        use nix::sys::signal::{kill, Signal};
        if self.reaped.load(Ordering::Acquire) {
            return Err(AgentOSError::HalError(
                "the audio player has already exited".into(),
            ));
        }
        let signal = match signal {
            PlayerSignal::Pause => Signal::SIGSTOP,
            PlayerSignal::Resume => Signal::SIGCONT,
            PlayerSignal::Interrupt => Signal::SIGINT,
            PlayerSignal::Kill => Signal::SIGKILL,
        };
        kill(nix::unistd::Pid::from_raw(self.pid), signal).map_err(|error| {
            AgentOSError::HalError(format!("Failed to signal audio player: {error}"))
        })
    }
}

pub(crate) struct SystemPlayer {
    child: Child,
    signaller: Arc<dyn PlayerSignaller>,
    reaped: Arc<AtomicBool>,
    /// Reader task draining the child's stderr pipe. Joined by `wait` so the
    /// libsndfile diagnostic is complete before the exit code is judged — a
    /// pipe left unread also blocks the child once the buffer fills.
    stderr: Option<tokio::task::JoinHandle<String>>,
}

impl SystemPlayer {
    pub(crate) async fn spawn(program: &str, args: &[String]) -> Result<Self, AgentOSError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // Kernel shutdown drops the supervisor tasks, which drops the
            // children — without this the player would keep the speakers for
            // the rest of the track with nothing left to stop it.
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                AgentOSError::HalError(format!("Failed to spawn '{program}': {error}"))
            })?;

        let pid = child.id().ok_or_else(|| {
            AgentOSError::HalError("Audio player exited before it could be supervised".into())
        })? as i32;

        let stderr = child.stderr.take().map(|pipe| {
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let _ = pipe.take(MAX_STDERR_BYTES).read_to_end(&mut buffer).await;
                String::from_utf8_lossy(&buffer).to_string()
            })
        });

        let reaped = Arc::new(AtomicBool::new(false));
        Ok(Self {
            child,
            signaller: Arc::new(PidSignaller {
                pid,
                reaped: Arc::clone(&reaped),
            }),
            reaped,
            stderr,
        })
    }
}

#[async_trait]
impl Player for SystemPlayer {
    fn signaller(&self) -> Arc<dyn PlayerSignaller> {
        Arc::clone(&self.signaller)
    }

    async fn wait(&mut self) -> PlayerExit {
        let code = match self.child.wait().await {
            Ok(status) => status.code().unwrap_or(-1),
            Err(error) => {
                self.reaped.store(true, Ordering::Release);
                return PlayerExit {
                    code: -1,
                    stderr: format!("failed to await audio player: {error}"),
                };
            }
        };
        // Synchronous, before any further `.await`: from here the PID may be
        // recycled, so no signal must reach it.
        self.reaped.store(true, Ordering::Release);
        let stderr = match self.stderr.take() {
            Some(handle) => handle.await.unwrap_or_default(),
            None => String::new(),
        };
        PlayerExit { code, stderr }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaybackState {
    Playing,
    Paused,
    /// Ran to the end of the track.
    Finished,
    /// Stopped on request.
    Stopped,
    /// Cut short by `max_seconds`, or by sitting paused past [`MAX_PAUSE`].
    Truncated,
    Failed,
}

impl PlaybackState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::Finished => "finished",
            Self::Stopped => "stopped",
            Self::Truncated => "truncated",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        !matches!(self, Self::Playing | Self::Paused)
    }
}

pub(crate) struct PlaybackSession {
    pub id: String,
    /// The agent that started it. Only this agent may control or see it —
    /// this ownership check, not the approval prompt, is what authorises the
    /// lifecycle actions.
    pub agent_id: String,
    pub audio_path: String,
    pub sink: Option<String>,
    pub max_seconds: u64,
    pub state: PlaybackState,
    pub transcoded: bool,
    pub error: Option<String>,
    /// Time spent actually playing, excluding paused spans.
    played: Duration,
    started_at: Instant,
    playing_since: Option<Instant>,
    ended_at: Option<Instant>,
    commands: mpsc::Sender<Request>,
    /// Fired once when the session reaches a terminal state, so a `wait: true`
    /// playback can block without polling.
    done: Arc<Notify>,
}

impl PlaybackSession {
    fn played(&self) -> Duration {
        match self.playing_since {
            Some(since) => self.played + since.elapsed(),
            None => self.played,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "playback_id": self.id,
            "state": self.state.as_str(),
            "audio_path": self.audio_path,
            "sink": self.sink,
            "played_seconds": self.played().as_secs(),
            "max_seconds": self.max_seconds,
            "transcoded": self.transcoded,
            "error": self.error,
        })
    }
}

/// Live playback sessions, keyed by `playback_id`.
///
/// In-memory by design: a kernel restart kills the players (`kill_on_drop`), so
/// a persisted row could only ever describe a process that no longer exists.
#[derive(Clone, Default)]
pub(crate) struct SessionRegistry {
    inner: Arc<Mutex<HashMap<String, PlaybackSession>>>,
}

impl SessionRegistry {
    /// Poisoning is recovered rather than propagated: a panic inside one
    /// `update` closure must not turn every later audio call into a panic.
    fn lock(&self) -> MutexGuard<'_, HashMap<String, PlaybackSession>> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Drop terminal sessions past their retention window. Called on every
    /// mutation; the map is bounded by [`MAX_PLAYBACKS_TOTAL`] plus whatever
    /// finished in the last ten minutes, so a full scan is the cheap option.
    fn prune(sessions: &mut HashMap<String, PlaybackSession>) {
        sessions.retain(|_, session| match session.ended_at {
            Some(ended) => ended.elapsed() < TERMINAL_RETENTION,
            None => true,
        });
    }

    /// Register a live session, refusing when either cap is already met.
    ///
    /// Prune, both cap checks and the insert happen under one lock so the
    /// ceiling is a real invariant rather than a check that concurrent calls
    /// can all pass at once.
    fn insert(&self, session: PlaybackSession) -> Result<(), AgentOSError> {
        let mut sessions = self.lock();
        Self::prune(&mut sessions);

        let live =
            |sessions: &HashMap<String, PlaybackSession>, agent: Option<&str>| -> Vec<String> {
                sessions
                    .values()
                    .filter(|candidate| !candidate.state.is_terminal())
                    .filter(|candidate| agent.is_none_or(|agent| candidate.agent_id == agent))
                    .map(|candidate| candidate.id.clone())
                    .collect()
            };

        let mine = live(&sessions, Some(&session.agent_id));
        if mine.len() >= MAX_PLAYBACKS_PER_AGENT {
            return Err(AgentOSError::HalError(format!(
                "Already playing {MAX_PLAYBACKS_PER_AGENT} tracks; stop one first (yours: {})",
                mine.join(", ")
            )));
        }
        // Deliberately counts without naming them: another agent's session ids
        // are not this caller's to see.
        if live(&sessions, None).len() >= MAX_PLAYBACKS_TOTAL {
            return Err(AgentOSError::HalError(format!(
                "The host is already playing {MAX_PLAYBACKS_TOTAL} tracks; try again shortly"
            )));
        }

        sessions.insert(session.id.clone(), session);
        Ok(())
    }

    /// Apply `edit` to a session the supervisor owns. Unlike the agent-facing
    /// lookups this performs no ownership check — the caller is the kernel.
    fn update(&self, id: &str, edit: impl FnOnce(&mut PlaybackSession)) {
        let mut sessions = self.lock();
        if let Some(session) = sessions.get_mut(id) {
            edit(session);
        }
    }

    /// Look a session up on behalf of `agent_id`.
    ///
    /// A session owned by another agent reports as *not found*, deliberately:
    /// "exists but is not yours" would let one agent enumerate another's
    /// activity by probing ids.
    pub(crate) fn get_owned(&self, id: &str, agent_id: &str) -> Result<Value, AgentOSError> {
        self.lock()
            .get(id)
            .filter(|session| session.agent_id == agent_id)
            .map(PlaybackSession::to_json)
            .ok_or_else(|| AgentOSError::HalError(format!("No playback session '{id}'")))
    }

    pub(crate) fn list_owned(&self, agent_id: &str) -> Vec<Value> {
        let mut sessions = self.lock();
        Self::prune(&mut sessions);
        let mut rows: Vec<&PlaybackSession> = sessions
            .values()
            .filter(|session| session.agent_id == agent_id)
            .collect();
        // Newest first: the id an agent has lost track of is usually the last
        // one it started. Keyed on start time, not played time — a long-paused
        // session has accumulated less of the latter than a fresh one.
        rows.sort_by_key(|session| std::cmp::Reverse(session.started_at));
        rows.into_iter().map(PlaybackSession::to_json).collect()
    }

    /// Ids of this agent's non-terminal sessions — the targets of a bare `stop`.
    pub(crate) fn active_owned_ids(&self, agent_id: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .lock()
            .values()
            .filter(|session| session.agent_id == agent_id && !session.state.is_terminal())
            .map(|session| session.id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Completion signal for a session this agent owns, or `None` when there is
    /// no such session.
    pub(crate) fn completion(&self, id: &str, agent_id: &str) -> Option<Arc<Notify>> {
        self.lock()
            .get(id)
            .filter(|session| session.agent_id == agent_id)
            .map(|session| Arc::clone(&session.done))
    }

    /// Channel for sending `command` to a session this agent owns.
    ///
    /// Returns the sender rather than sending, so the caller can `await` the
    /// send without holding the registry lock across it.
    fn command_channel(
        &self,
        id: &str,
        agent_id: &str,
        command: PlayerCommand,
    ) -> Result<mpsc::Sender<Request>, AgentOSError> {
        let sessions = self.lock();
        let session = sessions
            .get(id)
            .filter(|session| session.agent_id == agent_id)
            .ok_or_else(|| AgentOSError::HalError(format!("No playback session '{id}'")))?;
        if session.state.is_terminal() {
            return Err(AgentOSError::HalError(format!(
                "Playback session '{id}' already {}",
                session.state.as_str()
            )));
        }
        // Pausing a paused session would deliver a second SIGSTOP (harmless but
        // confusing), and resuming a playing one hides a lost SIGSTOP. Reject
        // both so the agent sees the real state.
        match (command, session.state) {
            (PlayerCommand::Pause, PlaybackState::Paused) => {
                return Err(AgentOSError::HalError(format!(
                    "Playback session '{id}' is already paused"
                )))
            }
            (PlayerCommand::Resume, PlaybackState::Playing) => {
                return Err(AgentOSError::HalError(format!(
                    "Playback session '{id}' is already playing"
                )))
            }
            _ => {}
        }
        Ok(session.commands.clone())
    }

    /// Deliver `command` to a session owned by `agent_id` and report the state
    /// the session actually reached.
    pub(crate) async fn command(
        &self,
        id: &str,
        agent_id: &str,
        command: PlayerCommand,
    ) -> Result<Value, AgentOSError> {
        let channel = self.command_channel(id, agent_id, command)?;
        let (ack, acked) = oneshot::channel();
        // Bounded: a supervisor busy in a transcode services no commands, and
        // an unbounded send on a full queue is the parked turn this whole
        // change exists to remove.
        tokio::time::timeout(COMMAND_ACK_TIMEOUT, channel.send((command, ack)))
            .await
            .map_err(|_| {
                AgentOSError::HalError(format!(
                    "Playback session '{id}' is not accepting commands right now"
                ))
            })?
            .map_err(|_| {
                AgentOSError::HalError(format!("Playback session '{id}' is no longer supervised"))
            })?;

        match tokio::time::timeout(COMMAND_ACK_TIMEOUT, acked).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(detail))) => {
                return Err(AgentOSError::HalError(format!(
                    "Could not {} playback session '{id}': {detail}",
                    command.as_str()
                )))
            }
            // Supervisor dropped the ack (it ended first) — the state read
            // below is still the truth.
            Ok(Err(_)) => {}
            Err(_) => tracing::warn!(
                playback_id = %id,
                command = command.as_str(),
                "Playback supervisor did not acknowledge the command in time"
            ),
        }

        let mut state = self.get_owned(id, agent_id)?;
        if let Some(object) = state.as_object_mut() {
            object.insert("requested".to_string(), json!(command.as_str()));
        }
        Ok(state)
    }
}

/// Deadline arithmetic for `max_seconds`, extracted so it is testable without
/// a clock: a paused span must not count against the cap, or pausing a track
/// for longer than its remaining budget would silently kill it on resume.
pub(crate) fn deadline_after_pause(deadline: Instant, paused_for: Duration) -> Instant {
    deadline + paused_for
}

/// Removes its file when dropped, including when the supervisor task is
/// dropped at kernel shutdown — otherwise a transcoded copy of a 100 MB track
/// is left in `/tmp` with nothing that will ever clean it.
struct ScratchFile(PathBuf);

impl Drop for ScratchFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Marks a session failed if the supervisor ever leaves without finishing it.
///
/// Covers the paths no explicit `finish` can: a panic inside the supervisor,
/// and the registry dropping the command sender. A non-terminal session is
/// never pruned and permanently consumes a playback slot, so "the supervisor
/// vanished" has to become a terminal state rather than a wedge.
struct SessionGuard {
    registry: SessionRegistry,
    id: String,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let mut done = None;
        self.registry.update(&self.id, |session| {
            if session.state.is_terminal() {
                return;
            }
            session.state = PlaybackState::Failed;
            session.error = Some("the playback supervisor stopped unexpectedly".to_string());
            session.ended_at = Some(Instant::now());
            done = Some(Arc::clone(&session.done));
        });
        if let Some(done) = done {
            done.notify_waiters();
        }
    }
}

/// The live player: its signaller, and the single `wait()` future that is
/// polled by reference for the life of the process.
struct Running {
    signaller: Arc<dyn PlayerSignaller>,
    waiting: BoxFuture<'static, PlayerExit>,
}

fn arm(mut player: Box<dyn Player>) -> Running {
    let signaller = player.signaller();
    Running {
        signaller,
        // Owning the player inside the future is what keeps it alive exactly
        // as long as the wait does, so `kill_on_drop` still fires when the
        // supervisor goes away.
        waiting: Box::pin(async move { player.wait().await }),
    }
}

/// Everything the supervisor needs to run and, if libsndfile refuses the
/// container, re-run the player after a transcode.
pub(crate) struct SupervisorSpec {
    pub id: String,
    pub player: Box<dyn Player>,
    /// Player argv WITHOUT the trailing input path, so a retry can re-use the
    /// flags with a different file.
    pub flags: Vec<String>,
    pub audio_path: PathBuf,
    pub max_seconds: u64,
    pub commands: mpsc::Receiver<Request>,
}

enum Outcome {
    Exited(PlayerExit),
    CapReached,
    PauseExpired,
    Command(PlayerCommand, oneshot::Sender<Result<(), String>>),
    Abandoned,
}

/// Own one player for its whole life: apply lifecycle commands, enforce the
/// duration cap, transcode-and-retry an unsupported container, and record the
/// outcome.
pub(crate) async fn supervise(
    registry: SessionRegistry,
    spawner: Arc<dyn PlayerSpawner>,
    transcoder: Arc<dyn Transcoder>,
    spec: SupervisorSpec,
) {
    let SupervisorSpec {
        id,
        player,
        flags,
        audio_path,
        max_seconds,
        mut commands,
    } = spec;

    let _guard = SessionGuard {
        registry: registry.clone(),
        id: id.clone(),
    };

    let mut running = arm(player);
    let mut deadline = Instant::now() + Duration::from_secs(max_seconds);
    let mut paused_at: Option<Instant> = None;
    let mut retried = false;
    // Held for the rest of the supervisor: dropping it unlinks the file.
    let mut scratch: Option<ScratchFile> = None;

    loop {
        let pause_deadline = paused_at.map(|at| at + MAX_PAUSE);

        let outcome = tokio::select! {
            // `&mut`, never a fresh future: another branch winning suspends
            // this one instead of dropping it, so an exit can never be
            // observed-then-discarded and the child is reaped exactly once.
            exit = &mut running.waiting => Outcome::Exited(exit),
            // Guarded rather than pushed into the future: a paused track must
            // not burn its budget, and a disabled branch is how `select!`
            // expresses "this cannot fire right now".
            _ = tokio::time::sleep_until(deadline), if paused_at.is_none() => Outcome::CapReached,
            _ = tokio::time::sleep_until(pause_deadline.unwrap_or(deadline)),
                if pause_deadline.is_some() => Outcome::PauseExpired,
            request = commands.recv() => match request {
                Some((command, ack)) => Outcome::Command(command, ack),
                // The registry dropped the sender: the session is gone.
                None => Outcome::Abandoned,
            },
        };

        match outcome {
            Outcome::Exited(exit) => {
                if exit.code != 0 && is_unsupported_format(&exit.stderr) && !retried {
                    // pw-play decodes through libsndfile, which has no
                    // MP3/AAC/M4A support in most builds. Triggered by the
                    // error, never by the extension: a `.bin` that is really a
                    // WAV still plays, and a `.wav` that is really an MP3 is
                    // still transcoded.
                    retried = true;
                    match replay_transcoded(
                        &registry,
                        &spawner,
                        &transcoder,
                        &id,
                        &audio_path,
                        &flags,
                    )
                    .await
                    {
                        Ok((next, temp)) => {
                            running = arm(next);
                            scratch = Some(temp);
                            continue;
                        }
                        Err(detail) => finish(&registry, &id, PlaybackState::Failed, Some(detail)),
                    }
                } else if exit.code == 0 {
                    finish(&registry, &id, PlaybackState::Finished, None);
                } else {
                    let detail = exit.stderr.trim();
                    let detail = if detail.is_empty() {
                        format!("pw-play exited with status {}", exit.code)
                    } else {
                        detail.to_string()
                    };
                    tracing::warn!(playback_id = %id, detail = %detail, "Audio playback failed");
                    finish(&registry, &id, PlaybackState::Failed, Some(detail));
                }
                break;
            }
            Outcome::CapReached => {
                stop_player(&mut running).await;
                finish(&registry, &id, PlaybackState::Truncated, None);
                break;
            }
            Outcome::PauseExpired => {
                // A SIGSTOPped process does not act on SIGINT until it is
                // continued.
                let _ = running.signaller.signal(PlayerSignal::Resume);
                stop_player(&mut running).await;
                finish(
                    &registry,
                    &id,
                    PlaybackState::Truncated,
                    Some(format!(
                        "stopped after sitting paused for {} minutes",
                        MAX_PAUSE.as_secs() / 60
                    )),
                );
                break;
            }
            Outcome::Command(PlayerCommand::Pause, ack) => {
                if let Err(error) = running.signaller.signal(PlayerSignal::Pause) {
                    tracing::warn!(playback_id = %id, %error, "Could not pause audio playback");
                    let _ = ack.send(Err(error.to_string()));
                    continue;
                }
                paused_at = Some(Instant::now());
                registry.update(&id, |session| {
                    if let Some(since) = session.playing_since.take() {
                        session.played += since.elapsed();
                    }
                    session.state = PlaybackState::Paused;
                });
                let _ = ack.send(Ok(()));
            }
            Outcome::Command(PlayerCommand::Resume, ack) => {
                if let Err(error) = running.signaller.signal(PlayerSignal::Resume) {
                    tracing::warn!(playback_id = %id, %error, "Could not resume audio playback");
                    let _ = ack.send(Err(error.to_string()));
                    continue;
                }
                if let Some(since) = paused_at.take() {
                    deadline = deadline_after_pause(deadline, since.elapsed());
                }
                registry.update(&id, |session| {
                    session.playing_since = Some(Instant::now());
                    session.state = PlaybackState::Playing;
                });
                let _ = ack.send(Ok(()));
            }
            Outcome::Command(PlayerCommand::Stop, ack) => {
                // Same SIGSTOP/SIGINT interaction as above: stopping a paused
                // track has to wake it first or the stop silently does nothing.
                if paused_at.take().is_some() {
                    let _ = running.signaller.signal(PlayerSignal::Resume);
                }
                stop_player(&mut running).await;
                finish(&registry, &id, PlaybackState::Stopped, None);
                let _ = ack.send(Ok(()));
                break;
            }
            Outcome::Abandoned => {
                // The `SessionGuard` marks it failed; nothing else can.
                break;
            }
        }
    }

    drop(scratch);
}

/// Decode the container libsndfile refused and start a player on the copy.
///
/// Returns the scratch file alongside the player so it is unlinked when the
/// supervisor ends, however it ends.
async fn replay_transcoded(
    registry: &SessionRegistry,
    spawner: &Arc<dyn PlayerSpawner>,
    transcoder: &Arc<dyn Transcoder>,
    id: &str,
    audio_path: &std::path::Path,
    flags: &[String],
) -> Result<(Box<dyn Player>, ScratchFile), String> {
    let temp = ScratchFile(
        std::env::temp_dir().join(format!("agentos-playback-{}.wav", uuid::Uuid::new_v4())),
    );
    // Bounded: the supervisor is outside its `select!` for the whole transcode,
    // so an ffmpeg that never returns is a session that can never be stopped.
    match tokio::time::timeout(TRANSCODE_TIMEOUT, transcoder.to_wav(audio_path, &temp.0)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return Err(format!(
                "audio format unsupported by pw-play and the ffmpeg transcode failed: {error}"
            ))
        }
        Err(_) => {
            return Err(format!(
                "audio format unsupported by pw-play and the ffmpeg transcode did not finish \
                 within {}s",
                TRANSCODE_TIMEOUT.as_secs()
            ))
        }
    }

    let mut args = flags.to_vec();
    args.push(temp.0.display().to_string());
    let player = spawner
        .spawn("pw-play", &args)
        .await
        .map_err(|error| error.to_string())?;
    registry.update(id, |session| session.transcoded = true);
    Ok((player, temp))
}

/// Interrupt a player and wait for it to actually go away.
///
/// Dropping the `Player` would also kill it (`kill_on_drop`), but only once the
/// supervisor returns — an unreaped child in the meantime is a zombie holding
/// the PipeWire stream open.
async fn stop_player(running: &mut Running) {
    if running.signaller.signal(PlayerSignal::Interrupt).is_err() {
        // Already gone; still drive the wait to completion so it is reaped.
        let _ = tokio::time::timeout(STOP_GRACE, &mut running.waiting).await;
        return;
    }
    if tokio::time::timeout(STOP_GRACE, &mut running.waiting)
        .await
        .is_err()
    {
        if let Err(error) = running.signaller.signal(PlayerSignal::Kill) {
            tracing::warn!(%error, "Audio player ignored SIGINT and could not be killed");
        }
        let _ = tokio::time::timeout(STOP_GRACE, &mut running.waiting).await;
    }
}

fn finish(registry: &SessionRegistry, id: &str, state: PlaybackState, error: Option<String>) {
    let mut done = None;
    registry.update(id, |session| {
        if let Some(since) = session.playing_since.take() {
            session.played += since.elapsed();
        }
        session.state = state;
        session.error = error;
        session.ended_at = Some(Instant::now());
        done = Some(Arc::clone(&session.done));
    });
    // Outside the registry lock: `notify_waiters` wakes tasks that immediately
    // re-read the map.
    if let Some(done) = done {
        done.notify_waiters();
    }
}

/// The one blocking helper the supervisor still needs: decoding a container
/// libsndfile cannot read. Behind a trait so the supervisor is testable
/// without ffmpeg on the box.
#[async_trait]
pub(crate) trait Transcoder: Send + Sync {
    async fn to_wav(
        &self,
        input: &std::path::Path,
        output: &std::path::Path,
    ) -> Result<(), AgentOSError>;
}

/// Register a new session and hand its supervisor to the runtime.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_session(
    registry: &SessionRegistry,
    spawner: Arc<dyn PlayerSpawner>,
    transcoder: Arc<dyn Transcoder>,
    agent_id: &str,
    audio_path: PathBuf,
    sink: Option<String>,
    max_seconds: u64,
    player: Box<dyn Player>,
    flags: Vec<String>,
) -> Result<String, AgentOSError> {
    let id = uuid::Uuid::new_v4().to_string();
    // Depth 4: the supervisor drains commands promptly, and a deeper queue only
    // buys an agent the chance to stack contradictory pause/resume pairs.
    let (tx, rx) = mpsc::channel(4);

    // Refused here rather than before spawning so the cap check and the insert
    // share one lock. The rejected player is dropped on return, and
    // `kill_on_drop` stops it.
    registry.insert(PlaybackSession {
        id: id.clone(),
        agent_id: agent_id.to_string(),
        audio_path: audio_path.display().to_string(),
        sink,
        max_seconds,
        state: PlaybackState::Playing,
        transcoded: false,
        error: None,
        played: Duration::ZERO,
        started_at: Instant::now(),
        playing_since: Some(Instant::now()),
        ended_at: None,
        commands: tx,
        done: Arc::new(Notify::new()),
    })?;

    tokio::spawn(supervise(
        registry.clone(),
        spawner,
        transcoder,
        SupervisorSpec {
            id: id.clone(),
            player,
            flags,
            audio_path,
            max_seconds,
            commands: rx,
        },
    ));

    Ok(id)
}

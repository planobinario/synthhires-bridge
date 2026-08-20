use chrono::{DateTime, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::process::Child;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskKind {
    ShellExec,
    FileRead,
    FileWrite,
    DbProxy,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Cancelling,
    Completed(Option<i32>),
    Failed(String),
    Killed,
}

#[derive(Debug, Clone)]
pub struct TaskState {
    pub id: Uuid,
    pub kind: TaskKind,
    pub description: String,
    pub status: TaskStatus,
    pub started_at_instant: std::time::Instant,
    pub started_at_utc: DateTime<Utc>,
    pub finished_at: Option<std::time::Instant>,
}

pub enum TaskHandle {
    ChildProcess(Box<Child>),
    CancellableFuture(CancellationToken),
}

type SharedTasks = Arc<Mutex<Vec<TaskState>>>;
type SharedCancellations = Arc<Mutex<HashMap<Uuid, CancellationToken>>>;

static SHARED_TASKS: OnceLock<SharedTasks> = OnceLock::new();
static SHARED_CANCELLATIONS: OnceLock<SharedCancellations> = OnceLock::new();

fn shared_tasks() -> SharedTasks {
    SHARED_TASKS
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .clone()
}

fn shared_cancellations() -> SharedCancellations {
    SHARED_CANCELLATIONS
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

/// Register the cancellation signal owned by the task executor.
/// The native UI's kill loop calls `cancel_global_task` through the
/// TaskRegistry, so the UI never owns or forcefully drops a process.
pub fn register_global_cancellation(id: Uuid, token: CancellationToken) {
    if let Ok(mut cancellations) = shared_cancellations().lock() {
        cancellations.insert(id, token);
    }
}

pub fn cancel_global_task(id: Uuid) -> bool {
    shared_cancellations()
        .lock()
        .ok()
        .and_then(|cancellations| cancellations.get(&id).cloned())
        .map(|token| {
            token.cancel();
            true
        })
        .unwrap_or(false)
}

pub fn unregister_global_cancellation(id: Uuid) {
    if let Ok(mut cancellations) = shared_cancellations().lock() {
        cancellations.remove(&id);
    }
}

/// Register an action produced by the WS client. The UI registry imports this
/// snapshot on its next refresh tick; no cross-thread UI references are kept.
pub fn record_global_task(state: TaskState) {
    if let Ok(mut tasks) = shared_tasks().lock() {
        if let Some(existing) = tasks.iter_mut().find(|task| task.id == state.id) {
            *existing = state;
        } else {
            tasks.push(state);
        }
        if tasks.len() > 200 {
            let remove = tasks.len() - 200;
            tasks.drain(..remove);
        }
    }
}

pub fn update_global_task(id: Uuid, status: TaskStatus) {
    if let Ok(mut tasks) = shared_tasks().lock() {
        if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
            task.status = status.clone();
            task.finished_at = is_terminal(&status).then(std::time::Instant::now);
        }
    }
    if is_terminal(&status) {
        unregister_global_cancellation(id);
    }
}

pub fn finish_global_task(id: Uuid, status: TaskStatus) {
    update_global_task(id, status);
}

fn is_terminal(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed(_) | TaskStatus::Failed(_) | TaskStatus::Killed
    )
}

pub struct TaskRegistry {
    max_capacity: usize,
    states: VecDeque<TaskState>,
    handles: HashMap<Uuid, TaskHandle>,
    shared: SharedTasks,
}

impl TaskRegistry {
    pub fn new(max_capacity: usize) -> Self {
        Self {
            max_capacity,
            states: VecDeque::with_capacity(max_capacity),
            handles: HashMap::new(),
            shared: shared_tasks(),
        }
    }

    pub fn spawn_task(&mut self, state: TaskState, handle: TaskHandle) {
        if self.states.len() >= self.max_capacity {
            self.states.pop_front();
        }
        self.handles.insert(state.id, handle);
        record_global_task(state.clone());
        self.states.push_back(state);
    }

    /// Refresh the UI-facing local queue from actions received by WsClient.
    /// The mutable receiver lets us preserve the existing iterator API.
    pub fn states(&mut self) -> impl Iterator<Item = &TaskState> {
        if let Ok(tasks) = self.shared.lock() {
            self.states = tasks.iter().cloned().collect();
        }
        self.states.iter()
    }

    /// Request cancellation. For WS actions this only sends the signal and
    /// leaves the task in `Cancelling` until the executor confirms that the
    /// child process has actually exited and emits the final result.
    pub async fn kill_task(&mut self, id: Uuid) -> bool {
        if let Some(mut handle) = self.handles.remove(&id) {
            match handle {
                TaskHandle::ChildProcess(ref mut child) => {
                    let _ = child.kill().await;
                    self.mark_status(id, TaskStatus::Killed);
                }
                TaskHandle::CancellableFuture(token) => {
                    token.cancel();
                    self.mark_status(id, TaskStatus::Cancelling);
                }
            }
            return true;
        }

        if cancel_global_task(id) {
            self.mark_status(id, TaskStatus::Cancelling);
            return true;
        }

        false
    }

    pub fn mark_status(&mut self, id: Uuid, status: TaskStatus) {
        if let Some(state) = self.states.iter_mut().find(|state| state.id == id) {
            state.status = status.clone();
            state.finished_at = is_terminal(&status).then(std::time::Instant::now);
        }
        update_global_task(id, status.clone());
        if is_terminal(&status) {
            self.handles.remove(&id);
        }
    }

    pub fn cleanup_stale_tasks(&mut self, ttl: std::time::Duration) {
        let now = std::time::Instant::now();
        self.states.retain(|state| {
            if let Some(finished_at) = state.finished_at {
                now.duration_since(finished_at) < ttl
            } else {
                true
            }
        });
        if let Ok(mut tasks) = self.shared.lock() {
            tasks.retain(|state| {
                if let Some(finished_at) = state.finished_at {
                    now.duration_since(finished_at) < ttl
                } else {
                    true
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_keeps_task_running_until_executor_finishes() {
        let id = Uuid::new_v4();
        let token = CancellationToken::new();
        register_global_cancellation(id, token.clone());
        record_global_task(TaskState {
            id,
            kind: TaskKind::ShellExec,
            description: "test shell".into(),
            status: TaskStatus::Running,
            started_at_instant: std::time::Instant::now(),
            started_at_utc: Utc::now(),
            finished_at: None,
        });

        let mut registry = TaskRegistry::new(10);
        assert!(registry.kill_task(id).await);
        assert!(token.is_cancelled());
        let state = registry.states().find(|state| state.id == id).unwrap();
        assert_eq!(state.status, TaskStatus::Cancelling);
        assert!(state.finished_at.is_none());

        finish_global_task(id, TaskStatus::Killed);
        let state = registry.states().find(|state| state.id == id).unwrap();
        assert_eq!(state.status, TaskStatus::Killed);
        assert!(state.finished_at.is_some());
    }
}

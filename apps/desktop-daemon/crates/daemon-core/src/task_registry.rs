use chrono::{DateTime, Utc};
use std::collections::{HashMap, VecDeque};
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
    ChildProcess(Child),
    CancellableFuture(CancellationToken),
}

pub struct TaskRegistry {
    max_capacity: usize,
    states: VecDeque<TaskState>,
    handles: HashMap<Uuid, TaskHandle>,
}

impl TaskRegistry {
    pub fn new(max_capacity: usize) -> Self {
        Self {
            max_capacity,
            states: VecDeque::with_capacity(max_capacity),
            handles: HashMap::new(),
        }
    }

    pub fn spawn_task(&mut self, state: TaskState, handle: TaskHandle) {
        if self.states.len() >= self.max_capacity {
            self.states.pop_front();
        }
        self.handles.insert(state.id, handle);
        self.states.push_back(state);
    }

    pub fn states(&self) -> impl Iterator<Item = &TaskState> {
        self.states.iter()
    }

    pub async fn kill_task(&mut self, id: Uuid) {
        if let Some(mut handle) = self.handles.remove(&id) {
            match handle {
                TaskHandle::ChildProcess(ref mut child) => {
                    let _ = child.kill().await;
                }
                TaskHandle::CancellableFuture(token) => {
                    token.cancel();
                }
            }
            self.mark_status(id, TaskStatus::Killed);
        }
    }

    pub fn mark_status(&mut self, id: Uuid, status: TaskStatus) {
        if let Some(state) = self.states.iter_mut().find(|s| s.id == id) {
            state.status = status.clone();
            state.finished_at = Some(std::time::Instant::now());
        }
        if !matches!(status, TaskStatus::Running) {
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
    }
}

//! A tiny in-memory priority task queue used as a shared codebase for the
//! swarm-development demo. Deliberately compact: most definitions live in this
//! one file so that concurrent agents collide in the same file.

/// Priority of a task. Higher sorts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Low,
    Normal,
    High,
}

/// Map a raw urgency score to a priority band.
pub fn priority_of(urgency: u32) -> Priority {
    if urgency >= 80 {
        Priority::High
    } else if urgency >= 40 {
        Priority::Normal
    } else {
        Priority::Low
    }
}

/// A single unit of work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: u64,
    pub name: String,
    pub priority: Priority,
}

/// An in-memory priority queue of tasks.
#[derive(Debug, Default)]
pub struct TaskQueue {
    tasks: Vec<Task>,
}

impl TaskQueue {
    /// Create an empty queue.
    pub fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    /// Add a task, keeping the queue sorted by priority (highest first).
    pub fn push(&mut self, task: Task) {
        self.tasks.push(task);
        self.tasks.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    /// Remove and return the highest-priority task.
    pub fn pop(&mut self) -> Option<Task> {
        if self.tasks.is_empty() {
            None
        } else {
            Some(self.tasks.remove(0))
        }
    }

    /// Number of tasks currently queued.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }
}

use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    Running,
    Paused,
    Cancelled,
}

#[derive(Debug)]
struct OperationControlInner {
    state: Mutex<OperationState>,
    changed: Condvar,
}

#[derive(Debug, Clone)]
pub struct OperationControl {
    inner: Arc<OperationControlInner>,
}

impl Default for OperationControl {
    fn default() -> Self {
        Self::new()
    }
}

impl OperationControl {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(OperationControlInner {
                state: Mutex::new(OperationState::Running),
                changed: Condvar::new(),
            }),
        }
    }

    pub fn state(&self) -> OperationState {
        *self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn pause(&self) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *state != OperationState::Running {
            return false;
        }
        *state = OperationState::Paused;
        true
    }

    pub fn resume(&self) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *state != OperationState::Paused {
            return false;
        }
        *state = OperationState::Running;
        self.inner.changed.notify_all();
        true
    }

    pub fn cancel(&self) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *state == OperationState::Cancelled {
            return false;
        }
        *state = OperationState::Cancelled;
        self.inner.changed.notify_all();
        true
    }

    /// Waits while paused and returns false once cancellation has been requested.
    pub fn checkpoint(&self) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while *state == OperationState::Paused {
            state = self
                .inner
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        *state != OperationState::Cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn pause_blocks_a_checkpoint_until_resume() {
        let control = OperationControl::new();
        assert!(control.pause());
        let worker_control = control.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || sender.send(worker_control.checkpoint()).unwrap());

        assert!(receiver.recv_timeout(Duration::from_millis(40)).is_err());
        assert!(control.resume());
        assert!(receiver.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn cancel_releases_a_paused_checkpoint_with_stop_signal() {
        let control = OperationControl::new();
        control.pause();
        let worker_control = control.clone();
        let worker = thread::spawn(move || worker_control.checkpoint());

        control.cancel();
        assert!(!worker.join().unwrap());
        assert_eq!(control.state(), OperationState::Cancelled);
    }
}

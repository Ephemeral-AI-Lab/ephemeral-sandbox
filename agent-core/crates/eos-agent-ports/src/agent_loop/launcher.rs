//! Agent-loop launcher contract.

use tokio::sync::{oneshot, watch};

use super::{AgentLoopOutcome, StartAgentLoopRequest};

/// Public non-blocking launcher for agent loops.
pub trait AgentLoopLauncher: Send + Sync {
    /// Start an agent loop and return immediately with its outcome receiver.
    fn start_agent_loop(&self, request: StartAgentLoopRequest) -> StartedAgentLoop;
}

/// Handle returned after an agent loop has been started.
#[derive(Debug)]
pub struct StartedAgentLoop {
    /// Receives the terminal loop outcome.
    pub outcome_receiver: oneshot::Receiver<AgentLoopOutcome>,
    /// Cooperative cancellation handle for the running loop.
    pub cancel_handle: AgentLoopCancelHandle,
}

/// Cooperative cancellation handle for one agent loop.
#[derive(Clone, Debug)]
pub struct AgentLoopCancelHandle {
    sender: watch::Sender<Option<String>>,
}

impl AgentLoopCancelHandle {
    /// Request loop cancellation. The first reason wins.
    pub fn cancel(&self, reason: impl Into<String>) {
        if self.sender.borrow().is_none() {
            let _ignored = self.sender.send(Some(reason.into()));
        }
    }
}

/// Loop-side cancellation signal.
#[derive(Clone, Debug)]
pub struct AgentLoopCancelSignal {
    receiver: watch::Receiver<Option<String>>,
}

impl AgentLoopCancelSignal {
    /// Current cancellation reason, if cancellation has been requested.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        self.receiver.borrow().clone()
    }
}

/// Build a cancel handle/signal pair for one loop.
#[must_use]
pub fn agent_loop_cancel_pair() -> (AgentLoopCancelHandle, AgentLoopCancelSignal) {
    let (sender, receiver) = watch::channel(None);
    (
        AgentLoopCancelHandle { sender },
        AgentLoopCancelSignal { receiver },
    )
}

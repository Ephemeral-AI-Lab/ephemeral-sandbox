use std::io;
use std::sync::Arc;

use crate::error::NamespaceExecutionError;
use crate::types::NamespaceExecutionId;
use crate::promise::{CompletionPromise, CompletionWaiter};
use crate::pty::PtyMaster;

pub struct ExecutionHandle<T> {
    id: NamespaceExecutionId,
    promise: Arc<CompletionPromise<T>>,
}

impl<T> ExecutionHandle<T> {
    pub fn new(id: NamespaceExecutionId, promise: Arc<CompletionPromise<T>>) -> Self {
        Self { id, promise }
    }

    pub fn id(&self) -> &NamespaceExecutionId {
        &self.id
    }

    pub fn is_finished(&self) -> bool {
        self.promise.is_resolved()
    }

    pub fn completion(&self) -> Arc<dyn CompletionWaiter>
    where
        T: Send + 'static,
    {
        upcast_waiter(Arc::clone(&self.promise))
    }

    pub fn resolved(&self) -> Option<Result<T, NamespaceExecutionError>>
    where
        T: Clone,
    {
        self.promise.resolved()
    }

    pub fn wait(self) -> Result<T, NamespaceExecutionError> {
        self.promise.wait()
    }
}

fn upcast_waiter<T: Send + 'static>(
    promise: Arc<CompletionPromise<T>>,
) -> Arc<dyn CompletionWaiter> {
    promise
}

pub struct InteractiveExecution<T> {
    exec: ExecutionHandle<T>,
    pty: PtyMaster,
}

impl<T> InteractiveExecution<T> {
    pub fn new(exec: ExecutionHandle<T>, pty: PtyMaster) -> Self {
        Self { exec, pty }
    }

    pub fn id(&self) -> &NamespaceExecutionId {
        self.exec.id()
    }

    pub fn is_finished(&self) -> bool {
        self.exec.is_finished()
    }

    pub fn completion(&self) -> Arc<dyn CompletionWaiter>
    where
        T: Send + 'static,
    {
        self.exec.completion()
    }

    pub fn resolved(&self) -> Option<Result<T, NamespaceExecutionError>>
    where
        T: Clone,
    {
        self.exec.resolved()
    }

    pub fn write_stdin(&self, bytes: &[u8]) -> io::Result<()> {
        self.pty.write_stdin(bytes)
    }

    pub fn pgid(&self) -> Option<i32> {
        self.pty.pgid()
    }

    pub fn output_len(&self) -> u64 {
        self.pty.output_len()
    }

    pub fn cancel(&self) {
        self.pty.cancel();
    }

    pub fn cancel_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.pty.cancel_handle()
    }

    pub fn wait(self) -> Result<T, NamespaceExecutionError> {
        self.exec.wait()
    }
}

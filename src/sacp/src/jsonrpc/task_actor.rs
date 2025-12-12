use std::panic::Location;

use futures::{
    FutureExt, StreamExt,
    channel::mpsc,
    future::BoxFuture,
    stream::{FusedStream, FuturesUnordered},
};

use crate::JrConnectionCx;
use crate::role::JrRole;

pub type TaskTx = mpsc::UnboundedSender<Task>;

#[must_use]
pub(crate) struct Task {
    future: BoxFuture<'static, Result<(), crate::Error>>,
}

impl Task {
    pub fn new(
        location: &'static Location<'static>,
        task_future: impl IntoFuture<Output = Result<(), crate::Error>, IntoFuture: Send + 'static>,
    ) -> Self {
        let task_future = task_future.into_future();
        Task {
            future: futures::FutureExt::map(
                task_future,
                |result| match result {
                    Ok(()) => Ok(()),
                    Err(err) => {
                        let data = err.data.clone();
                        Err(err.with_data(serde_json::json! {
                            {
                                "spawned_at": format!("{}:{}:{}", location.file(), location.line(), location.column()),
                                "data": data,
                            }
                        }))
                    }
                }
            ).boxed()
        }
    }

    /// Return a new task that executes with the given name
    fn named(self, name: Option<String>) -> Task {
        if let Some(name) = name {
            Task {
                future: crate::util::instrumented_with_connection_name(name, self.future).boxed(),
            }
        } else {
            self
        }
    }

    pub fn spawn(self, task_tx: &TaskTx) -> Result<(), crate::Error> {
        task_tx
            .unbounded_send(self)
            .map_err(crate::util::internal_error)?;
        Ok(())
    }
}

/// The "task actor" manages other tasks
pub(super) async fn task_actor(
    mut task_rx: mpsc::UnboundedReceiver<Task>,
) -> Result<(), crate::Error> {
    let mut futures = FuturesUnordered::new();

    loop {
        // If we have no futures to run, wait until we do.
        if futures.is_empty() {
            match task_rx.next().await {
                Some(task) => futures.push(task.future),
                None => return Ok(()),
            }
            continue;
        }

        // If there are no more tasks coming in, just drain our queue and return.
        if task_rx.is_terminated() {
            while let Some(result) = futures.next().await {
                result?;
            }
            return Ok(());
        }

        // Otherwise, run futures until we get a request for a new task.
        futures::select! {
            result = futures.next() => if let Some(result) = result {
                result?;
            },

            task = task_rx.next() => {
                if let Some(task) = task {
                    futures.push(task.future);
                }
            }
        }
    }
}

pub(crate) struct PendingTask<Role: JrRole> {
    task_fn: Box<dyn PendingTaskFn<Role>>,
}

impl<Role: JrRole> PendingTask<Role> {
    pub fn new<Fut>(
        location: &'static Location<'static>,
        task_function: impl FnOnce(JrConnectionCx<Role>) -> Fut + Send + 'static,
    ) -> Self
    where
        Fut: Future<Output = Result<(), crate::Error>> + Send + 'static,
    {
        PendingTask {
            task_fn: Box::new(move |cx| Task::new(location, task_function(cx))),
        }
    }

    /// Return a new pending task that will execute with the given name
    pub fn named(self, name: Option<String>) -> Self {
        PendingTask {
            task_fn: Box::new(move |cx| self.into_task(cx).named(name)),
        }
    }

    pub fn into_task(self, cx: JrConnectionCx<Role>) -> Task {
        self.task_fn.into_task(cx)
    }
}

trait PendingTaskFn<Role: JrRole>: 'static + Send {
    fn into_task(self: Box<Self>, cx: JrConnectionCx<Role>) -> Task;
}

impl<Role: JrRole, F> PendingTaskFn<Role> for F
where
    F: FnOnce(JrConnectionCx<Role>) -> Task + 'static + Send,
{
    fn into_task(self: Box<Self>, cx: JrConnectionCx<Role>) -> Task {
        (*self)(cx)
    }
}

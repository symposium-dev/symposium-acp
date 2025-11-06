use std::panic::Location;

use futures::{
    StreamExt,
    channel::mpsc,
    future::BoxFuture,
    stream::{FusedStream, FuturesUnordered},
};

use crate::JrConnectionCx;

pub(crate) struct Task<'scope> {
    location: &'static Location<'static>,
    task_future: BoxFuture<'scope, Result<(), crate::Error>>,
}

impl<'scope> Task<'scope> {
    pub fn new(
        location: &'static Location<'static>,
        task_future: impl Future<Output = Result<(), crate::Error>> + Send + 'scope,
    ) -> Self {
        Task {
            location,
            task_future: Box::pin(task_future),
        }
    }

    /// Return a new task that executes with the given name
    fn named(self, name: Option<String>) -> Task<'scope> {
        if let Some(name) = name {
            let Task {
                location,
                task_future,
            } = self;
            Task {
                location,
                task_future: Box::pin(crate::util::instrumented_with_connection_name(
                    name,
                    task_future,
                )),
            }
        } else {
            self
        }
    }
}

/// The "task actor" manages other tasks
pub(super) async fn task_actor(
    mut task_rx: mpsc::UnboundedReceiver<Task<'_>>,
) -> Result<(), crate::Error> {
    let mut futures = FuturesUnordered::new();

    loop {
        // If we have no futures to run, wait until we do.
        if futures.is_empty() {
            match task_rx.next().await {
                Some(task) => futures.push(execute_task(task)),
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
                    futures.push(execute_task(task));
                }
            }
        }
    }
}

async fn execute_task(task: Task<'_>) -> Result<(), crate::Error> {
    let Task {
        location,
        task_future,
    } = task;
    task_future.await.map_err(|err| {
        let data = err.data.clone();
        err.with_data(serde_json::json! {
            {
                "spawned_at": format!("{}:{}:{}", location.file(), location.line(), location.column()),
                "data": data,
            }
        })
    })
}

pub(crate) struct PendingTask<'scope> {
    task_fn: Box<dyn PendingTaskFn<'scope>>,
}

impl<'scope> PendingTask<'scope> {
    pub fn new<Fut>(
        location: &'static Location<'static>,
        task_function: impl FnOnce(JrConnectionCx) -> Fut + Send + 'scope,
    ) -> Self
    where
        Fut: Future<Output = Result<(), crate::Error>> + Send + 'scope,
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

    pub fn into_task(self, cx: JrConnectionCx) -> Task<'scope> {
        self.task_fn.into_task(cx)
    }
}

trait PendingTaskFn<'scope>: 'scope + Send {
    fn into_task(self: Box<Self>, cx: JrConnectionCx) -> Task<'scope>;
}

impl<'scope, F> PendingTaskFn<'scope> for F
where
    F: FnOnce(JrConnectionCx) -> Task<'scope> + 'scope + Send,
{
    fn into_task(self: Box<Self>, cx: JrConnectionCx) -> Task<'scope> {
        (*self)(cx)
    }
}

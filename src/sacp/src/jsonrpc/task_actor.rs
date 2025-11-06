use std::{panic::Location, pin::Pin};

use futures::{
    StreamExt,
    channel::mpsc,
    stream::{FusedStream, FuturesUnordered},
};

use crate::JrConnectionCx;

/// The "task actor" manages other tasks
pub(super) async fn task_actor(
    mut task_rx: mpsc::UnboundedReceiver<Task>,
    cx: &JrConnectionCx,
) -> Result<(), crate::Error> {
    let mut futures = FuturesUnordered::new();

    loop {
        // If we have no futures to run, wait until we do.
        if futures.is_empty() {
            match task_rx.next().await {
                Some(task) => futures.push(execute_task(task, cx.clone())),
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
                    futures.push(execute_task(task, cx.clone()));
                }
            }
        }
    }
}

async fn execute_task(task: Task, cx: JrConnectionCx) -> Result<(), crate::Error> {
    let Task { location, task_fn } = task;
    task_fn.run(cx).await.map_err(|err| {
        let data = err.data.clone();
        err.with_data(serde_json::json! {
            {
                "spawned_at": format!("{}:{}:{}", location.file(), location.line(), location.column()),
                "data": data,
            }
        })
    })
}

pub(crate) struct Task {
    location: &'static Location<'static>,
    task_fn: Box<dyn TaskFn + Send>,
}

impl Task {
    pub fn new<F, Fut>(location: &'static Location<'static>, task_function: F) -> Self
    where
        F: FnOnce(JrConnectionCx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), crate::Error>> + Send + 'static,
    {
        Task {
            location,
            task_fn: Box::new(FnOnceTask(task_function)),
        }
    }
}

trait TaskFn {
    fn run(
        self: Box<Self>,
        cx: JrConnectionCx,
    ) -> Pin<Box<dyn Future<Output = Result<(), crate::Error>>>>;
}

struct FnOnceTask<F, Fut>(F)
where
    F: FnOnce(JrConnectionCx) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), crate::Error>> + Send + 'static;

impl<F, Fut> TaskFn for FnOnceTask<F, Fut>
where
    F: FnOnce(JrConnectionCx) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), crate::Error>> + Send + 'static,
{
    fn run(
        self: Box<Self>,
        cx: JrConnectionCx,
    ) -> Pin<Box<dyn Future<Output = Result<(), crate::Error>>>> {
        Box::pin(self.0(cx))
    }
}

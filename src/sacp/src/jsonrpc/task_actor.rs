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

    pub fn spawn(self, task_tx: &TaskTx) -> Result<(), crate::Error> {
        task_tx
            .unbounded_send(self)
            .map_err(crate::util::internal_error)?;
        Ok(())
    }
}

/// The "task actor" manages dynamically spawned tasks.
pub(super) async fn task_actor<Role: JrRole>(
    mut task_rx: mpsc::UnboundedReceiver<Task>,
    _cx: &JrConnectionCx<Role>,
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

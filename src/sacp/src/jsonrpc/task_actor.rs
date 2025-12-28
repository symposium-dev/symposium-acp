use std::panic::Location;

use futures::{FutureExt, channel::mpsc, future::BoxFuture};

use crate::JrConnectionCx;
use crate::link::JrLink;
use crate::util::process_stream_concurrently;

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
                },
            )
            .boxed()
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
pub(super) async fn task_actor<Link: JrLink>(
    task_rx: mpsc::UnboundedReceiver<Task>,
    _cx: &JrConnectionCx<Link>,
) -> Result<(), crate::Error> {
    process_stream_concurrently(
        task_rx,
        async |task| task.future.await,
        |a, b| Box::pin(a(b)),
    )
    .await
}

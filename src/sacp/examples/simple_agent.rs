use sacp::schema::{AgentCapabilities, InitializeRequest, InitializeResponse};
use sacp::{Agent, Client, ConnectionTo, Dispatch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[tokio::main]
async fn main() -> Result<(), sacp::Error> {
    Agent.builder()
        .name("my-agent") // for debugging
        .on_receive_request(
            async move |initialize: InitializeRequest, responder, _connection| {
                // Respond to initialize successfully
                responder.respond(
                    InitializeResponse::new(initialize.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            sacp::on_receive_request!(),
        )
        .on_receive_dispatch(
            async move |message: Dispatch, cx: ConnectionTo<Client>| {
                // Respond to any other message with an error
                message.respond_with_error(sacp::util::internal_error("TODO"), cx)
            },
            sacp::on_receive_dispatch!(),
        )
        .connect_to(sacp::ByteStreams::new(
            tokio::io::stdout().compat_write(),
            tokio::io::stdin().compat(),
        ))
        .await
}

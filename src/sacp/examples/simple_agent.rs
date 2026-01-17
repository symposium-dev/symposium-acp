use sacp::AgentToClient;
use sacp::schema::{AgentCapabilities, InitializeRequest, InitializeResponse};
use sacp::{ConnectionTo, MessageCx};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[tokio::main]
async fn main() -> Result<(), sacp::Error> {
    AgentToClient::builder()
        .name("my-agent") // for debugging
        .on_receive_request(
            async move |initialize: InitializeRequest, request_cx, _connection_cx| {
                // Respond to initialize successfully
                request_cx.respond(
                    InitializeResponse::new(initialize.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            sacp::on_receive_request!(),
        )
        .on_receive_message(
            async move |message: MessageCx, cx: ConnectionTo<AgentToClient>| {
                // Respond to any other message with an error
                message.respond_with_error(sacp::util::internal_error("TODO"), cx)
            },
            sacp::on_receive_message!(),
        )
        .serve(sacp::ByteStreams::new(
            tokio::io::stdout().compat_write(),
            tokio::io::stdin().compat(),
        ))
        .await
}

use sacp::Role;
use sacp::role::UntypedRole;
use sacp::util::MatchDispatch;
use sacp::{
    ConnectionTo, HandleMessageFrom, Handled, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse,
    Dispatch, Responder, ConnectTo, util::MatchDispatchFrom,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EchoRequestResponse {
    text: Vec<String>,
}

impl JsonRpcMessage for EchoRequestResponse {
    fn matches_method(method: &str) -> bool {
        method == "echo"
    }

    fn method(&self) -> &str {
        "echo"
    }

    fn to_untyped_message(&self) -> Result<sacp::UntypedMessage, sacp::Error> {
        Ok(sacp::UntypedMessage {
            method: self.method().to_string(),
            params: sacp::util::json_cast(self)?,
        })
    }

    fn parse_message(method: &str, params: &impl serde::Serialize) -> Result<Self, sacp::Error> {
        if !<Self as JsonRpcMessage>::matches_method(method) {
            return Err(sacp::Error::method_not_found());
        }
        sacp::util::json_cast(params)
    }
}

impl JsonRpcResponse for EchoRequestResponse {
    fn into_json(self, _method: &str) -> Result<serde_json::Value, sacp::Error> {
        sacp::util::json_cast(self)
    }

    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, sacp::Error> {
        sacp::util::json_cast(value)
    }
}

impl JsonRpcRequest for EchoRequestResponse {
    type Response = EchoRequestResponse;
}

struct EchoHandler;

impl<Counterpart: Role> HandleMessageFrom<Counterpart> for EchoHandler {
    async fn handle_message_from(
        &mut self,
        message: Dispatch,
        _connection: ConnectionTo<Counterpart>,
    ) -> Result<Handled<Dispatch>, sacp::Error> {
        MatchDispatch::new(message)
            .if_request(async move |request: EchoRequestResponse, responder| {
                responder.respond(request)
            })
            .await
            .done()
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "TestHandler"
    }
}

#[tokio::test]
async fn modify_message_en_route() -> Result<(), sacp::Error> {
    // Demonstrate a case where we modify a message
    // using a `HandleMessageFrom` invoked from `MatchDispatch`

    struct TestComponent;

    impl ConnectTo<UntypedRole> for TestComponent {
        async fn connect_to(self, client: impl ConnectTo<UntypedRole>) -> Result<(), sacp::Error> {
            UntypedRole.connect_from()
                .with_handler(PushHandler {
                    message: "b".to_string(),
                })
                .with_handler(EchoHandler)
                .connect_to(client)
                .await
        }
    }

    struct PushHandler {
        message: String,
    }

    impl HandleMessageFrom<UntypedRole> for PushHandler {
        async fn handle_message_from(
            &mut self,
            message: Dispatch,
            cx: ConnectionTo<UntypedRole>,
        ) -> Result<Handled<Dispatch>, sacp::Error> {
            MatchDispatchFrom::new(message, &cx)
                .if_request(async move |mut request: EchoRequestResponse, responder| {
                    request.text.push(self.message.clone());
                    Ok(Handled::No {
                        message: (request, responder),
                        retry: false,
                    })
                })
                .await
                .done()
        }

        fn describe_chain(&self) -> impl std::fmt::Debug {
            "TestHandler"
        }
    }

    UntypedRole.connect_from()
        .connect_with(TestComponent, async |cx| {
            let result = cx
                .send_request(EchoRequestResponse {
                    text: vec!["a".to_string()],
                })
                .block_task()
                .await?;

            expect_test::expect![[r#"
                EchoRequestResponse {
                    text: [
                        "a",
                        "b",
                    ],
                }
            "#]]
            .assert_debug_eq(&result);
            Ok(())
        })
        .await
}

#[tokio::test]
async fn modify_message_en_route_inline() -> Result<(), sacp::Error> {
    // Demonstrate a case where we modify a message en route using an `on_receive_request` call

    struct TestComponent;

    impl ConnectTo<UntypedRole> for TestComponent {
        async fn connect_to(self, client: impl ConnectTo<UntypedRole>) -> Result<(), sacp::Error> {
            UntypedRole.connect_from()
                .on_receive_request(
                    async move |mut request: EchoRequestResponse,
                                responder: Responder<EchoRequestResponse>,
                                _connection: ConnectionTo<UntypedRole>| {
                        request.text.push("b".to_string());
                        Ok(Handled::No {
                            message: (request, responder),
                            retry: false,
                        })
                    },
                    sacp::on_receive_request!(),
                )
                .with_handler(EchoHandler)
                .connect_to(client)
                .await
        }
    }

    UntypedRole.connect_from()
        .connect_with(TestComponent, async |cx| {
            let result = cx
                .send_request(EchoRequestResponse {
                    text: vec!["a".to_string()],
                })
                .block_task()
                .await?;

            expect_test::expect![[r#"
                EchoRequestResponse {
                    text: [
                        "a",
                        "b",
                    ],
                }
            "#]]
            .assert_debug_eq(&result);
            Ok(())
        })
        .await
}

#[tokio::test]
async fn modify_message_and_stop() -> Result<(), sacp::Error> {
    // Demonstrate a case where we have an async handler that just returns `()`
    // in front (and hence we never see the `'b`).

    struct TestComponent;

    impl ConnectTo<UntypedRole> for TestComponent {
        async fn connect_to(self, client: impl ConnectTo<UntypedRole>) -> Result<(), sacp::Error> {
            UntypedRole.connect_from()
                .on_receive_request(
                    async move |request: EchoRequestResponse,
                                responder: Responder<EchoRequestResponse>,
                                _connection: ConnectionTo<UntypedRole>| {
                        responder.respond(request)
                    },
                    sacp::on_receive_request!(),
                )
                .on_receive_request(
                    async move |mut request: EchoRequestResponse,
                                responder: Responder<EchoRequestResponse>,
                                _connection: ConnectionTo<UntypedRole>| {
                        request.text.push("b".to_string());
                        Ok(Handled::No {
                            message: (request, responder),
                            retry: false,
                        })
                    },
                    sacp::on_receive_request!(),
                )
                .with_handler(EchoHandler)
                .connect_to(client)
                .await
        }
    }

    UntypedRole.connect_from()
        .connect_with(TestComponent, async |cx| {
            let result = cx
                .send_request(EchoRequestResponse {
                    text: vec!["a".to_string()],
                })
                .block_task()
                .await?;

            expect_test::expect![[r#"
                EchoRequestResponse {
                    text: [
                        "a",
                    ],
                }
            "#]]
            .assert_debug_eq(&result);
            Ok(())
        })
        .await
}

use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use fxhash::FxHashMap;

use sacp::schema::{NewSessionRequest, NewSessionResponse, SessionId};
use sacp::util::MatchMessage;
use sacp::{
    Channel, Component, DynComponent, Handled, JrConnectionCx, JrHandlerChain, JrMessageHandler,
    JrRequestCx, MessageAndCx, UntypedMessage,
};
use std::sync::{Arc, Mutex};

use crate::mcp_server_builder::McpServer;
use crate::{
    McpConnectRequest, McpConnectResponse, McpDisconnectNotification, McpOverAcpNotification,
    McpOverAcpRequest, SuccessorNotification, SuccessorRequest,
};

/// Manages MCP services offered to successor proxies and agents.
///
/// Use the [`Self::add_mcp_server`] method to register MCP servers. For rmcp-based servers,
/// use the `sacp-rmcp` crate which provides convenient extension methods.
///
/// This struct is a handle to the underlying registry. Cloning the struct produces a second handle to the same registry.
///
/// # Handling requests
///
/// You must add the registry (or a clone of it) to the [`JrHandlerChain`] so that it can intercept MCP requests.
/// Typically you do this by providing it as an argument to the handler chain methods.
///
/// [`JrHandlerChain`]: sacp::JrHandlerChain
#[derive(Clone, Default, Debug)]
pub struct McpServiceRegistry {
    data: Arc<Mutex<McpServiceRegistryData>>,
}

#[derive(Default, Debug)]
struct McpServiceRegistryData {
    registered_by_name: FxHashMap<String, Arc<RegisteredMcpServer>>,
    registered_by_url: FxHashMap<String, Arc<RegisteredMcpServer>>,
    connections: FxHashMap<String, mpsc::Sender<MessageAndCx>>,
}

impl McpServiceRegistry {
    /// Creates a new empty MCP service registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an [`McpServer`] to the registry.
    ///
    /// This server will be added to all new sessions where this registry is in the handler chain.
    ///
    /// See the [`McpServer`] documentation for more information.
    pub fn with_mcp_server(
        self,
        name: impl ToString,
        server: McpServer,
    ) -> Result<Self, sacp::Error> {
        self.add_mcp_server_with_context(name, move |mcp_cx| server.new_connection(mcp_cx))?;
        Ok(self)
    }

    /// Add an MCP server to the registry using a custom constructor function.
    ///
    /// This server will be added to all new sessions where this registry is in the handler chain.
    ///
    /// This method is for independent MCP servers that do not make use of ACP.
    /// You may wish to use the `sacp-rmcp` crate which provides convenient
    /// extension methods for working with MCP servers implemented using the `rmcp` crate.
    ///
    /// # Parameters
    ///
    /// - `name`: The name of the server.
    /// - `new_fn`: Constructor function that creates the MCP server and returns a [`Component`] for connecting to it.
    pub fn add_mcp_server<C: Component>(
        &self,
        name: impl ToString,
        new_fn: impl Fn() -> C + Send + Sync + 'static,
    ) -> Result<(), sacp::Error> {
        struct FnSpawner<F> {
            new_fn: F,
        }

        impl<C, F> SpawnMcpServer for FnSpawner<F>
        where
            F: Fn() -> C + Send + Sync + 'static,
            C: Component,
        {
            fn spawn(&self, _cx: McpContext) -> DynComponent {
                let component = (self.new_fn)();
                DynComponent::new(component)
            }
        }

        self.add_mcp_server_internal(name, FnSpawner { new_fn })
    }

    /// Add an MCP server to the registry that wishes to receive a [`McpContext`] when created.
    ///
    /// This server will be added to all new sessions where this registry is in the handler chain.
    ///
    /// This method is for MCP servers that require information about the ACP connection and/or
    /// the ability to make ACP requests.
    ///
    /// # Parameters
    ///
    /// - `name`: The name of the server.
    /// - `new_fn`: Constructor function that creates the MCP server and returns a [`Component`] for connecting to it.
    pub fn add_mcp_server_with_context<C: Component>(
        &self,
        name: impl ToString,
        new_fn: impl Fn(McpContext) -> C + Send + Sync + 'static,
    ) -> Result<(), sacp::Error> {
        struct FnSpawner<F> {
            new_fn: F,
        }

        impl<C, F> SpawnMcpServer for FnSpawner<F>
        where
            F: Fn(McpContext) -> C + Send + Sync + 'static,
            C: Component,
        {
            fn spawn(&self, cx: McpContext) -> DynComponent {
                let component = (self.new_fn)(cx);
                DynComponent::new(component)
            }
        }

        self.add_mcp_server_internal(name, FnSpawner { new_fn })
    }
    fn add_mcp_server_internal(
        &self,
        name: impl ToString,
        spawner: impl SpawnMcpServer,
    ) -> Result<(), sacp::Error> {
        let name = name.to_string();
        if let Some(_) = self.get_registered_server_by_name(&name) {
            return Err(sacp::util::internal_error(format!(
                "Server with name '{}' already exists",
                name
            )));
        }

        let uuid = uuid::Uuid::new_v4().to_string();
        let service = Arc::new(RegisteredMcpServer {
            name,
            url: format!("acp:{uuid}"),
            spawn: Arc::new(spawner),
        });
        self.insert_registered_server(service);
        Ok(())
    }

    fn insert_registered_server(&self, service: Arc<RegisteredMcpServer>) {
        let mut data = self.data.lock().expect("not poisoned");
        data.registered_by_name
            .insert(service.name.clone(), service.clone());
        data.registered_by_url
            .insert(service.url.clone(), service.clone());
    }

    fn get_registered_server_by_name(&self, name: &str) -> Option<Arc<RegisteredMcpServer>> {
        self.data
            .lock()
            .expect("not poisoned")
            .registered_by_name
            .get(name)
            .cloned()
    }

    fn get_registered_server_by_url(&self, url: &str) -> Option<Arc<RegisteredMcpServer>> {
        self.data
            .lock()
            .expect("not poisoned")
            .registered_by_url
            .get(url)
            .cloned()
    }

    fn insert_connection(&self, connection_id: &str, tx: mpsc::Sender<sacp::MessageAndCx>) {
        self.data
            .lock()
            .expect("not poisoned")
            .connections
            .insert(connection_id.to_string(), tx);
    }

    fn get_connection(&self, connection_id: &str) -> Option<mpsc::Sender<sacp::MessageAndCx>> {
        self.data
            .lock()
            .expect("not poisoned")
            .connections
            .get(connection_id)
            .cloned()
    }

    fn remove_connection(&self, connection_id: &str) -> bool {
        self.data
            .lock()
            .expect("not poisoned")
            .connections
            .remove(connection_id)
            .is_some()
    }

    /// Adds all registered MCP servers to the given `NewSessionRequest`.
    ///
    /// This method appends the MCP server configurations for all servers registered
    /// with this registry to the `mcp_servers` field of the request. This is useful
    /// when you want to manually populate a request with MCP servers outside of the
    /// automatic handler chain processing.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let registry = McpServiceRegistry::new();
    /// registry.add_mcp_server("my-server", || MyMcpServer)?;
    ///
    /// let mut request = NewSessionRequest {

    /// registry.add_registered_mcp_servers_to(&mut request);
    /// // request.mcp_servers now contains "my-server"
    /// ```
    pub fn add_registered_mcp_servers_to(&self, request: &mut NewSessionRequest) {
        let data = self.data.lock().expect("not poisoned");
        for server in data.registered_by_url.values() {
            request.mcp_servers.push(server.acp_mcp_server());
        }
    }

    async fn handle_connect_request(
        &self,
        successor_request: SuccessorRequest<McpConnectRequest>,
        request_cx: JrRequestCx<McpConnectResponse>,
    ) -> Result<
        Handled<(
            SuccessorRequest<McpConnectRequest>,
            JrRequestCx<McpConnectResponse>,
        )>,
        sacp::Error,
    > {
        let SuccessorRequest { request, .. } = &successor_request;
        let outer_cx = request_cx.connection_cx();

        // Check if we have a registered server with the given URL. If not, don't try to handle the request.
        let Some(registered_server) = self.get_registered_server_by_url(&request.acp_url) else {
            return Ok(Handled::No((successor_request, request_cx)));
        };

        // Create a unique connection ID and a channel for future communication
        let connection_id = format!("mcp-over-acp-connection:{}", uuid::Uuid::new_v4());
        let (mcp_server_tx, mut mcp_server_rx) = mpsc::channel(128);
        self.insert_connection(&connection_id, mcp_server_tx);

        // Create connected channel pair for client-server communication
        let (client_channel, server_channel) = Channel::duplex();

        // Create client-side handler that wraps messages and forwards to successor
        let client_component = {
            let connection_id = connection_id.clone();
            let outer_cx = outer_cx.clone();

            JrHandlerChain::new()
                .on_receive_message(async move |message: sacp::MessageAndCx| {
                    // Wrap the message in McpOverAcp{Request,Notification} and forward to successor
                    let wrapped = message.map(
                        |request, request_cx| {
                            (
                                McpOverAcpRequest {
                                    connection_id: connection_id.clone(),
                                    request,
                                    meta: None,
                                },
                                request_cx,
                            )
                        },
                        |notification, cx| {
                            (
                                McpOverAcpNotification {
                                    connection_id: connection_id.clone(),
                                    notification,
                                    meta: None,
                                },
                                cx,
                            )
                        },
                    );
                    outer_cx.send_proxied_message(wrapped)
                })
                .with_spawned(move |mcp_cx| async move {
                    while let Some(msg) = mcp_server_rx.next().await {
                        mcp_cx.send_proxied_message(msg)?;
                    }
                    Ok(())
                })
        };

        // Get the MCP server component
        let mcp_server = registered_server.spawn.spawn(McpContext {
            session_id: request.session_id.clone(),
            connection_cx: outer_cx.clone(),
        });

        // Spawn both sides of the connection
        let spawn_results = outer_cx
            .spawn(async move { client_component.serve(client_channel).await })
            .and_then(|()| {
                // Spawn the MCP server serving the server channel
                request_cx
                    .connection_cx()
                    .spawn(async move { mcp_server.serve(server_channel).await })
            });

        match spawn_results {
            Ok(()) => {
                request_cx.respond(McpConnectResponse {
                    connection_id,
                    meta: None,
                })?;
                Ok(Handled::Yes)
            }

            Err(err) => {
                request_cx.respond_with_error(err)?;
                Ok(Handled::Yes)
            }
        }
    }

    async fn handle_mcp_over_acp_request(
        &self,
        successor_request: SuccessorRequest<McpOverAcpRequest<UntypedMessage>>,
        request_cx: JrRequestCx<serde_json::Value>,
    ) -> Result<
        Handled<(
            SuccessorRequest<McpOverAcpRequest<UntypedMessage>>,
            JrRequestCx<serde_json::Value>,
        )>,
        sacp::Error,
    > {
        // Check if we have a registered server with the given URL. If not, don't try to handle the request.
        let Some(mut mcp_server_tx) = self.get_connection(&successor_request.request.connection_id)
        else {
            return Ok(Handled::No((successor_request, request_cx)));
        };

        let SuccessorRequest { request, .. } = successor_request;

        mcp_server_tx
            .send(MessageAndCx::Request(request.request, request_cx))
            .await
            .map_err(sacp::Error::into_internal_error)?;

        Ok(Handled::Yes)
    }

    async fn handle_mcp_over_acp_notification(
        &self,
        successor_notification: SuccessorNotification<McpOverAcpNotification<UntypedMessage>>,
        notification_cx: JrConnectionCx,
    ) -> Result<
        Handled<(
            SuccessorNotification<McpOverAcpNotification<UntypedMessage>>,
            JrConnectionCx,
        )>,
        sacp::Error,
    > {
        // Check if we have a registered server with the given URL. If not, don't try to handle the request.
        let Some(mut mcp_server_tx) =
            self.get_connection(&successor_notification.notification.connection_id)
        else {
            return Ok(Handled::No((successor_notification, notification_cx)));
        };

        let SuccessorNotification { notification, .. } = successor_notification;

        mcp_server_tx
            .send(MessageAndCx::Notification(
                notification.notification,
                notification_cx.clone(),
            ))
            .await
            .map_err(sacp::Error::into_internal_error)?;

        Ok(Handled::Yes)
    }

    async fn handle_mcp_disconnect_notification(
        &self,
        successor_notification: SuccessorNotification<McpDisconnectNotification>,
        notification_cx: JrConnectionCx,
    ) -> Result<
        Handled<(
            SuccessorNotification<McpDisconnectNotification>,
            JrConnectionCx,
        )>,
        sacp::Error,
    > {
        let SuccessorNotification { notification, .. } = &successor_notification;

        // Remove connection if we have it. Otherwise, do not handle the notification.
        if self.remove_connection(&notification.connection_id) {
            Ok(Handled::Yes)
        } else {
            Ok(Handled::No((successor_notification, notification_cx)))
        }
    }

    async fn handle_new_session_request(
        &self,
        mut request: NewSessionRequest,
        request_cx: JrRequestCx<NewSessionResponse>,
    ) -> Result<Handled<(NewSessionRequest, JrRequestCx<NewSessionResponse>)>, sacp::Error> {
        // Add the MCP servers into the session/new request.
        //
        // Q: Do we care if there are already servers with that name?
        self.add_registered_mcp_servers_to(&mut request);

        // Return the modified request so subsequent handlers can see the MCP servers we added.
        Ok(Handled::No((request, request_cx)))
    }
}

impl JrMessageHandler for McpServiceRegistry {
    fn describe_chain(&self) -> impl std::fmt::Debug {
        "McpServiceRegistry"
    }

    async fn handle_message(
        &mut self,
        message: sacp::MessageAndCx,
    ) -> Result<sacp::Handled<sacp::MessageAndCx>, sacp::Error> {
        MatchMessage::new(message)
            .if_request(|request, request_cx| self.handle_connect_request(request, request_cx))
            .await
            .if_request(|request, request_cx| self.handle_mcp_over_acp_request(request, request_cx))
            .await
            .if_request(|request, request_cx| self.handle_new_session_request(request, request_cx))
            .await
            .if_notification(|notification, notification_cx| {
                self.handle_mcp_over_acp_notification(notification, notification_cx)
            })
            .await
            .if_notification(|notification, notification_cx| {
                self.handle_mcp_disconnect_notification(notification, notification_cx)
            })
            .await
            .done()
    }
}

/// A "registeed" MCP server can be launched when a connection is established.
#[derive(Clone)]
struct RegisteredMcpServer {
    name: String,
    url: String,
    spawn: Arc<dyn SpawnMcpServer>,
}

impl RegisteredMcpServer {
    fn acp_mcp_server(&self) -> sacp::schema::McpServer {
        sacp::schema::McpServer::Http {
            name: self.name.clone(),
            url: self.url.clone(),
            headers: vec![],
        }
    }
}

impl std::fmt::Debug for RegisteredMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredMcpServer")
            .field("name", &self.name)
            .field("url", &self.url)
            .finish()
    }
}

/// Trait for spawning MCP server components.
///
/// This trait allows creating MCP server instances that implement the `Component` trait.
trait SpawnMcpServer: Send + Sync + 'static {
    /// Create a new MCP server component.
    ///
    /// Returns a `DynComponent` that can be used with the Component API.
    fn spawn(&self, cx: McpContext) -> sacp::DynComponent;
}

impl AsRef<McpServiceRegistry> for McpServiceRegistry {
    fn as_ref(&self) -> &McpServiceRegistry {
        self
    }
}

/// Context about the ACP and MCP connection available to an MCP server.
#[derive(Clone)]
pub struct McpContext {
    session_id: SessionId,
    connection_cx: JrConnectionCx,
}

impl McpContext {
    /// The session-id of the session connected to this MCP server.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// The ACP connection context, which can be used to send ACP requests and notifications
    /// to your successor.
    pub fn connection_cx(&self) -> JrConnectionCx {
        self.connection_cx.clone()
    }
}

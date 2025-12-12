//! MCP service registry for managing MCP servers.

use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use fxhash::FxHashMap;

use crate::mcp::{McpClientToServer, McpServerEnd};
use crate::schema::{
    McpConnectRequest, McpConnectResponse, McpDisconnectNotification, McpOverAcpMessage,
    NewSessionRequest, NewSessionResponse,
};
use crate::util::MatchMessage;
use crate::{
    Agent, Channel, Component, DynComponent, Handled, HasEndpoint, JrConnectionCx,
    JrMessageHandlerSend, JrRequestCx, JrRole, MessageCx, UntypedMessage,
};
use std::sync::{Arc, Mutex};

use super::server::McpServer;

/// Manages MCP services offered to successor proxies and agents.
///
/// Use the [`Self::with_mcp_server`] method to register MCP servers.
///
/// This struct is a handle to the underlying registry. Cloning the struct produces a second handle to the same registry.
///
/// # Handling requests
///
/// You must add the registry (or a clone of it) to the [`JrConnectionBuilder`] so that it can intercept MCP requests.
/// Typically you do this by providing it as an argument to the handler chain methods.
///
/// [`JrConnectionBuilder`]: crate::JrConnectionBuilder
#[derive(Clone, Default, Debug)]
pub struct McpServiceRegistry<Role: JrRole> {
    data: Arc<Mutex<McpServiceRegistryData<Role>>>,
}

#[derive(Default, Debug)]
struct McpServiceRegistryData<Role: JrRole> {
    registered_by_name: FxHashMap<String, Arc<RegisteredMcpServer<Role>>>,
    registered_by_url: FxHashMap<String, Arc<RegisteredMcpServer<Role>>>,
    connections: FxHashMap<String, mpsc::Sender<MessageCx>>,
}

impl<Role: JrRole> McpServiceRegistry<Role>
where
    Role: HasEndpoint<Agent>,
{
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
        server: McpServer<Role>,
    ) -> Result<Self, crate::Error> {
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
    ) -> Result<(), crate::Error> {
        struct FnSpawner<F> {
            new_fn: F,
        }

        impl<Role, C, F> SpawnMcpServer<Role> for FnSpawner<F>
        where
            Role: JrRole,
            F: Fn() -> C + Send + Sync + 'static,
            C: Component,
        {
            fn spawn(&self, _cx: McpContext<Role>) -> DynComponent {
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
        new_fn: impl Fn(McpContext<Role>) -> C + Send + Sync + 'static,
    ) -> Result<(), crate::Error> {
        struct FnSpawner<F> {
            new_fn: F,
        }

        impl<Role, C, F> SpawnMcpServer<Role> for FnSpawner<F>
        where
            Role: JrRole,
            F: Fn(McpContext<Role>) -> C + Send + Sync + 'static,
            C: Component,
        {
            fn spawn(&self, cx: McpContext<Role>) -> DynComponent {
                let component = (self.new_fn)(cx);
                DynComponent::new(component)
            }
        }

        self.add_mcp_server_internal(name, FnSpawner { new_fn })
    }

    fn add_mcp_server_internal(
        &self,
        name: impl ToString,
        spawner: impl SpawnMcpServer<Role>,
    ) -> Result<(), crate::Error> {
        let name = name.to_string();
        if self.get_registered_server_by_name(&name).is_some() {
            return Err(crate::util::internal_error(format!(
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

    fn insert_registered_server(&self, service: Arc<RegisteredMcpServer<Role>>) {
        let mut data = self.data.lock().expect("not poisoned");
        data.registered_by_name
            .insert(service.name.clone(), service.clone());
        data.registered_by_url
            .insert(service.url.clone(), service.clone());
    }

    fn get_registered_server_by_name(&self, name: &str) -> Option<Arc<RegisteredMcpServer<Role>>> {
        self.data
            .lock()
            .expect("not poisoned")
            .registered_by_name
            .get(name)
            .cloned()
    }

    fn get_registered_server_by_url(&self, url: &str) -> Option<Arc<RegisteredMcpServer<Role>>> {
        self.data
            .lock()
            .expect("not poisoned")
            .registered_by_url
            .get(url)
            .cloned()
    }

    fn insert_connection(&self, connection_id: &str, tx: mpsc::Sender<MessageCx>) {
        self.data
            .lock()
            .expect("not poisoned")
            .connections
            .insert(connection_id.to_string(), tx);
    }

    fn get_connection(&self, connection_id: &str) -> Option<mpsc::Sender<MessageCx>> {
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
    pub fn add_registered_mcp_servers_to(&self, request: &mut NewSessionRequest) {
        let data = self.data.lock().expect("not poisoned");
        for server in data.registered_by_url.values() {
            request.mcp_servers.push(server.acp_mcp_server());
        }
    }

    async fn handle_connect_request(
        &self,
        request: McpConnectRequest,
        request_cx: JrRequestCx<McpConnectResponse>,
        outer_cx: JrConnectionCx<Role>,
    ) -> Result<Handled<(McpConnectRequest, JrRequestCx<McpConnectResponse>)>, crate::Error> {
        // Check if we have a registered server with the given URL. If not, don't try to handle the request.
        let Some(registered_server) = self.get_registered_server_by_url(&request.acp_url) else {
            return Ok(Handled::No((request, request_cx)));
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

            McpClientToServer::builder()
                .on_receive_message(async move |message: MessageCx, _mcp_cx| {
                    // Wrap the message in McpOverAcp{Request,Notification} and forward to successor
                    let wrapped = message.map(
                        |request, request_cx| {
                            (
                                McpOverAcpMessage {
                                    connection_id: connection_id.clone(),
                                    message: request,
                                    meta: None,
                                },
                                request_cx,
                            )
                        },
                        |notification| McpOverAcpMessage {
                            connection_id: connection_id.clone(),
                            message: notification,
                            meta: None,
                        },
                    );
                    outer_cx.send_proxied_message_to(Agent, wrapped)
                })
                .with_spawned(move |mcp_cx| async move {
                    // Messages we pull off this channel were sent from the agent.
                    // Forward them back to the MCP server.
                    while let Some(msg) = mcp_server_rx.next().await {
                        mcp_cx.send_proxied_message_to(McpServerEnd, msg)?;
                    }
                    Ok(())
                })
        };

        // Get the MCP server component
        let mcp_server = registered_server.spawn.spawn(McpContext {
            acp_url: request.acp_url.clone(),
            connection_cx: outer_cx.clone(),
        });

        // Spawn both sides of the connection
        let spawn_results = outer_cx
            .spawn(async move { client_component.serve(client_channel).await })
            .and_then(|()| {
                // Spawn the MCP server serving the server channel
                outer_cx.spawn(async move { mcp_server.serve(server_channel).await })
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
        request: McpOverAcpMessage<UntypedMessage>,
        request_cx: JrRequestCx<serde_json::Value>,
    ) -> Result<
        Handled<(
            McpOverAcpMessage<UntypedMessage>,
            JrRequestCx<serde_json::Value>,
        )>,
        crate::Error,
    > {
        // Check if we have a registered server with the given URL. If not, don't try to handle the request.
        let Some(mut mcp_server_tx) = self.get_connection(&request.connection_id) else {
            return Ok(Handled::No((request, request_cx)));
        };

        mcp_server_tx
            .send(MessageCx::Request(request.message, request_cx))
            .await
            .map_err(crate::Error::into_internal_error)?;

        Ok(Handled::Yes)
    }

    async fn handle_mcp_over_acp_notification(
        &self,
        notification: McpOverAcpMessage<UntypedMessage>,
    ) -> Result<Handled<McpOverAcpMessage<UntypedMessage>>, crate::Error> {
        // Check if we have a registered server with the given URL. If not, don't try to handle the request.
        let Some(mut mcp_server_tx) = self.get_connection(&notification.connection_id) else {
            return Ok(Handled::No(notification));
        };

        mcp_server_tx
            .send(MessageCx::Notification(notification.message))
            .await
            .map_err(crate::Error::into_internal_error)?;

        Ok(Handled::Yes)
    }

    async fn handle_mcp_disconnect_notification(
        &self,
        successor_notification: McpDisconnectNotification,
    ) -> Result<Handled<McpDisconnectNotification>, crate::Error> {
        // Remove connection if we have it. Otherwise, do not handle the notification.
        if self.remove_connection(&successor_notification.connection_id) {
            Ok(Handled::Yes)
        } else {
            Ok(Handled::No(successor_notification))
        }
    }

    async fn handle_new_session_request(
        &self,
        mut request: NewSessionRequest,
        request_cx: JrRequestCx<NewSessionResponse>,
    ) -> Result<Handled<(NewSessionRequest, JrRequestCx<NewSessionResponse>)>, crate::Error> {
        // Add the MCP servers into the session/new request.
        //
        // Q: Do we care if there are already servers with that name?
        self.add_registered_mcp_servers_to(&mut request);

        // Return the modified request so subsequent handlers can see the MCP servers we added.
        Ok(Handled::No((request, request_cx)))
    }
}

impl<Role: JrRole> JrMessageHandlerSend for McpServiceRegistry<Role>
where
    Role: HasEndpoint<Agent>,
{
    type Role = Role;

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "McpServiceRegistry"
    }

    async fn handle_message(
        &mut self,
        message: MessageCx,
        connection_cx: JrConnectionCx<Role>,
    ) -> Result<Handled<MessageCx>, crate::Error> {
        // Hmm, this is a bit wacky:
        //
        // * In a proxy, we expect to receive MCP over ACP notifications wrapped as a "FromSuccessorNotification"
        //   and we don't expect to receive them unwrapped (that would be the client sending it to us, not our agent,
        //   and that's weird);
        // * But in a *client*, we expect to receive incoming messages unwrapped (i.e., from our successor),
        //   and not wrapped (we don't expect *anything* wrapped).
        //
        // So we just accept them in either direction for now. The whole thing feels a bit inelegant,
        // but I guess it works.

        MatchMessage::new(message)
            // MCP connect requests come from the Agent direction (wrapped in SuccessorMessage)
            .if_request_from(Agent, connection_cx.clone(), |request, request_cx, cx| {
                self.handle_connect_request(request, request_cx, cx)
            })
            .await
            // MCP over ACP requests come from the Agent direction
            .if_request_from(Agent, connection_cx.clone(), |request, request_cx, _cx| {
                self.handle_mcp_over_acp_request(request, request_cx)
            })
            .await
            // session/new requests come from the Client direction (not wrapped)
            .if_request(|request, request_cx| self.handle_new_session_request(request, request_cx))
            .await
            // MCP over ACP notifications come from the Agent direction
            .if_notification_from(Agent, connection_cx.clone(), |notification, _cx| {
                self.handle_mcp_over_acp_notification(notification)
            })
            .await
            // MCP disconnect notifications come from the Agent direction
            .if_notification_from(Agent, connection_cx, |notification, _cx| {
                self.handle_mcp_disconnect_notification(notification)
            })
            .await
            .done()
    }
}

/// A "registered" MCP server can be launched when a connection is established.
#[derive(Clone)]
struct RegisteredMcpServer<Role: JrRole> {
    name: String,
    url: String,
    spawn: Arc<dyn SpawnMcpServer<Role>>,
}

impl<Role: JrRole> RegisteredMcpServer<Role> {
    fn acp_mcp_server(&self) -> crate::schema::McpServer {
        crate::schema::McpServer::Http {
            name: self.name.clone(),
            url: self.url.clone(),
            headers: vec![],
        }
    }
}

impl<Role: JrRole> std::fmt::Debug for RegisteredMcpServer<Role> {
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
trait SpawnMcpServer<Role: JrRole>: Send + Sync + 'static {
    /// Create a new MCP server component.
    ///
    /// Returns a `DynComponent` that can be used with the Component API.
    fn spawn(&self, cx: McpContext<Role>) -> DynComponent;
}

impl<Role: JrRole> AsRef<McpServiceRegistry<Role>> for McpServiceRegistry<Role>
where
    Role: HasEndpoint<Agent>,
{
    fn as_ref(&self) -> &McpServiceRegistry<Role> {
        self
    }
}

/// Context about the ACP and MCP connection available to an MCP server.
#[derive(Clone)]
pub struct McpContext<Role: JrRole> {
    acp_url: String,
    connection_cx: JrConnectionCx<Role>,
}

impl<Role: JrRole> McpContext<Role> {
    /// The `acp:UUID` that was given.
    pub fn acp_url(&self) -> String {
        self.acp_url.clone()
    }

    /// The ACP connection context, which can be used to send ACP requests and notifications
    /// to your successor.
    pub fn connection_cx(&self) -> JrConnectionCx<Role> {
        self.connection_cx.clone()
    }
}

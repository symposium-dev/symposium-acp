//! MCP service registry for managing MCP servers.

use fxhash::FxHashMap;
use uuid::Uuid;

use crate::jsonrpc::DynamicHandlerRegistration;
use crate::mcp_server::McpServer;
use crate::mcp_server::registry::active_session::McpServerSession;
use crate::schema::NewSessionRequest;
use crate::util::MatchMessageFrom;
use crate::{
    Agent, Client, Component, DynComponent, Handled, HasEndpoint, JrConnectionCx,
    JrMessageHandlerSend, JrRole, MessageCx,
};
use std::sync::Arc;

mod active_session;

/// Manages MCP services offered to successor proxies and agents.
///
/// Use the [`Self::with_mcp_server`] method to register MCP servers.
///
/// # Handling requests
///
/// You must add the registry (or a clone of it) to the [`JrConnectionBuilder`] so that it can intercept MCP requests.
/// Typically you do this by providing it as an argument to the connection builder methods.
///
/// [`JrConnectionBuilder`]: crate::JrConnectionBuilder
#[derive(Clone, Default, Debug)]
pub struct McpServiceRegistry<Role: JrRole> {
    registered_by_name: FxHashMap<String, Arc<RegisteredMcpServer<Role>>>,
    dynamic_handlers: Vec<DynamicHandlerRegistration<Role>>,
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
    /// This server will be added to all new sessions where this registry is registered on the connection.
    ///
    /// See the [`McpServer`] documentation for more information.
    pub fn with_mcp_server(
        self,
        name: impl ToString,
        server: McpServer<Role>,
    ) -> Result<Self, crate::Error> {
        self.with_custom_mcp_server(name, move |mcp_cx| server.new_connection(mcp_cx))
    }

    /// Add a custom MCP server to the registry that is not implemented with [`McpServer`].
    /// This method can be used to add (for example) an external MCP server.
    ///
    /// # Using the rmcp crate
    ///
    /// The `sacp-rmcp` crate allows adapting MCP servers implemented with the rmcp crate.
    ///
    /// # Parameters
    ///
    /// - `name`: The name of the server.
    /// - `new_fn`: Constructor function that creates the MCP server and returns a [`Component`] for connecting to it.
    pub fn with_custom_mcp_server<C: Component>(
        self,
        name: impl ToString,
        new_fn: impl Fn(McpContext<Role>) -> C + Send + Sync + 'static,
    ) -> Result<Self, crate::Error> {
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
        mut self,
        name: impl ToString,
        spawner: impl SpawnMcpServer<Role>,
    ) -> Result<Self, crate::Error> {
        let name = name.to_string();
        if self.get_registered_server_by_name(&name).is_some() {
            return Err(crate::util::internal_error(format!(
                "Server with name '{}' already exists",
                name
            )));
        }

        let service = Arc::new(RegisteredMcpServer {
            name,
            spawn: Arc::new(spawner),
        });
        self.insert_registered_server(service);
        Ok(self)
    }

    fn insert_registered_server(&mut self, service: Arc<RegisteredMcpServer<Role>>) {
        self.registered_by_name
            .insert(service.name.clone(), service.clone());
    }

    fn get_registered_server_by_name(&self, name: &str) -> Option<Arc<RegisteredMcpServer<Role>>> {
        self.registered_by_name.get(name).cloned()
    }

    /// Adds registered MCP servers to the given `NewSessionRequest`
    /// and register message handlers for subsequent messages with `cx`.
    /// The returned vector contains handles to these message handlers;
    /// when it is dropped, they will be de-registered.
    ///
    /// This method appends the MCP server configurations for all servers registered
    /// with this registry to the `mcp_servers` field of the request. This is useful
    /// when you want to manually populate a request with MCP servers outside of the
    /// automatic connection processing.
    pub fn add_registered_mcp_servers_to(
        &self,
        request: &mut NewSessionRequest,
        cx: &JrConnectionCx<Role>,
    ) -> Result<Vec<DynamicHandlerRegistration<Role>>, crate::Error>
    where
        Role: HasEndpoint<Agent>,
    {
        let mut result = Vec::with_capacity(self.registered_by_name.len());
        for server in self.registered_by_name.values() {
            let acp_url = format!("acp:{}", Uuid::new_v4());
            let connection =
                McpServerSession::new(Role::default(), acp_url.clone(), server.clone());
            request.mcp_servers.push(crate::schema::McpServer::Http {
                name: server.name.clone(),
                url: acp_url,
                headers: Default::default(),
            });
            result.push(cx.add_dynamic_handler(connection)?);
        }
        Ok(result)
    }
}

impl<Role: JrRole> JrMessageHandlerSend for McpServiceRegistry<Role>
where
    Role: HasEndpoint<Client> + HasEndpoint<Agent>,
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

        MatchMessageFrom::new(message, &connection_cx)
            // session/new requests come from the Client direction (not wrapped)
            .if_request_from(Client, async |mut request, request_cx| {
                // Add the MCP servers into the session/new request.
                //
                // Q: Do we care if there are already servers with that name?
                self.dynamic_handlers
                    .extend(self.add_registered_mcp_servers_to(&mut request, &connection_cx)?);

                // Return the modified request so subsequent handlers can see the MCP servers we added.
                Ok(Handled::No {
                    message: (request, request_cx),
                    retry: false,
                })
            })
            .await
            .done()
    }
}

/// A "registered" MCP server can be launched when a connection is established.
#[derive(Clone)]
struct RegisteredMcpServer<Role: JrRole> {
    name: String,
    spawn: Arc<dyn SpawnMcpServer<Role>>,
}

impl<Role: JrRole> std::fmt::Debug for RegisteredMcpServer<Role> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredMcpServer")
            .field("name", &self.name)
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

//! Component abstraction for agents and proxies.
//!
//! This module provides the [`Component`] trait that defines the interface for things
//! that can be run as part of a conductor's chain - agents, proxies, or any ACP-speaking component.
//!
//! ## Usage
//!
//! Components serve by forwarding to other components, creating a chain of message processors.
//! The type parameter `L` is the link type that this component can serve as a transport for.
//!
//! To implement a component, implement the `serve` method:
//!
//! ```rust,ignore
//! use sacp::component::Component;
//! use sacp::link::AgentToClient;
//!
//! struct MyAgent {
//!     // configuration fields
//! }
//!
//! // An agent serves as a transport for AgentToClient connections
//! impl Component<AgentToClient> for MyAgent {
//!     async fn serve(self, client: impl Component<AgentToClient::ConnectsTo>) -> Result<(), sacp::Error> {
//!         sacp::AgentToClient::builder()
//!             .name("my-agent")
//!             // configure handlers here
//!             .serve(client)
//!             .await
//!     }
//! }
//! ```

use futures::future::BoxFuture;
use std::{fmt::Debug, future::Future, marker::PhantomData};

use crate::{Channel, link::JrLink};

/// A component that can participate in the Agent-Client Protocol.
///
/// This trait represents anything that can communicate via JSON-RPC messages over channels -
/// agents, proxies, in-process connections, or any ACP-speaking component.
///
/// The type parameter `L` is the link type that this component can serve as a transport for.
/// For example:
/// - An agent implements `Component<AgentToClient>` - it serves connections where the
///   local side is an agent talking to a client
/// - A proxy implements `Component<ProxyToConductor>` - it serves connections where the
///   local side is a proxy talking to a conductor
/// - Transports like `Channel` implement `Component<L>` for all `L` since they're link-agnostic
///
/// # Component Types
///
/// The trait is implemented by several built-in types representing different communication patterns:
///
/// - **[`ByteStreams`]**: A component communicating over byte streams (stdin/stdout, sockets, etc.)
/// - **[`Channel`]**: A component communicating via in-process message channels (for testing or direct connections)
/// - **[`AcpAgent`]**: An external agent running in a separate process with stdio communication
/// - **Custom components**: Proxies, transformers, or any ACP-aware service
///
/// # Two Ways to Serve
///
/// Components can be used in two ways:
///
/// 1. **`serve(client)`** - Serve by forwarding to another component (most components implement this)
/// 2. **`into_server()`** - Convert into a channel endpoint and server future (base cases implement this)
///
/// Most components only need to implement `serve(client)` - the `into_server()` method has a default
/// implementation that creates an intermediate channel and calls `serve`.
///
/// # Implementation Example
///
/// ```rust,ignore
/// use sacp::{Component, peer::AgentToClient};
///
/// struct MyAgent {
///     config: AgentConfig,
/// }
///
/// impl Component<AgentToClient> for MyAgent {
///     async fn serve(self, client: impl Component<<AgentToClient as JrLink>::ConnectsTo>) -> Result<(), sacp::Error> {
///         // Set up connection that forwards to client
///         sacp::AgentToClient::builder()
///             .name("my-agent")
///             .on_receive_request(async |req: MyRequest, cx| {
///                 // Handle request
///                 cx.respond(MyResponse { status: "ok".into() })
///             })
///             .serve(client)
///             .await
///     }
/// }
/// ```
///
/// # Heterogeneous Collections
///
/// For storing different component types in the same collection, use [`DynComponent`]:
///
/// ```rust,ignore
/// use sacp::link::AgentToClient;
///
/// let components: Vec<DynComponent<AgentToClient>> = vec![
///     DynComponent::new(proxy1),
///     DynComponent::new(proxy2),
///     DynComponent::new(agent),
/// ];
/// ```
///
/// [`ByteStreams`]: crate::ByteStreams
/// [`AcpAgent`]: https://docs.rs/sacp-tokio/latest/sacp_tokio/struct.AcpAgent.html
/// [`JrConnectionBuilder`]: crate::JrConnectionBuilder
pub trait Component<L: JrLink>: Send + 'static {
    /// Serve this component by forwarding to a client component.
    ///
    /// Most components implement this method to set up their connection and
    /// forward messages to the provided client.
    ///
    /// # Arguments
    ///
    /// * `client` - The component to forward messages to (implements `Component<L::ConnectsTo>`)
    ///
    /// # Returns
    ///
    /// A future that resolves when the component stops serving, either successfully
    /// or with an error. The future must be `Send`.
    fn serve(
        self,
        client: impl Component<L::ConnectsTo>,
    ) -> impl Future<Output = Result<(), crate::Error>> + Send;

    /// Convert this component into a channel endpoint and server future.
    ///
    /// This method returns:
    /// - A `Channel` that can be used to communicate with this component
    /// - A `BoxFuture` that runs the component's server logic
    ///
    /// The default implementation creates an intermediate channel pair and calls `serve`
    /// on one endpoint while returning the other endpoint for the caller to use.
    ///
    /// Base cases like `Channel` and `ByteStreams` override this to avoid unnecessary copying.
    ///
    /// # Returns
    ///
    /// A tuple of `(Channel, BoxFuture)` where the channel is for the caller to use
    /// and the future must be spawned to run the server.
    fn into_server(self) -> (Channel, BoxFuture<'static, Result<(), crate::Error>>)
    where
        Self: Sized,
    {
        let (channel_a, channel_b) = Channel::duplex();
        let future = Box::pin(self.serve(channel_b));
        (channel_a, future)
    }
}

/// Type-erased component trait for object-safe dynamic dispatch.
///
/// This trait is internal and used by [`DynComponent`]. Users should implement
/// [`Component`] instead, which is automatically converted to `ErasedComponent`
/// via a blanket implementation.
trait ErasedComponent<L: JrLink>: Send {
    fn type_name(&self) -> String;

    fn serve_erased(
        self: Box<Self>,
        client: Box<dyn ErasedComponent<L::ConnectsTo>>,
    ) -> BoxFuture<'static, Result<(), crate::Error>>;

    fn into_server_erased(
        self: Box<Self>,
    ) -> (Channel, BoxFuture<'static, Result<(), crate::Error>>);
}

/// Blanket implementation: any `Component<L>` can be type-erased.
impl<C: Component<L>, L: JrLink> ErasedComponent<L> for C {
    fn type_name(&self) -> String {
        std::any::type_name::<C>().to_string()
    }

    fn serve_erased(
        self: Box<Self>,
        client: Box<dyn ErasedComponent<L::ConnectsTo>>,
    ) -> BoxFuture<'static, Result<(), crate::Error>> {
        Box::pin(async move {
            (*self)
                .serve(DynComponent {
                    inner: client,
                    _marker: PhantomData,
                })
                .await
        })
    }

    fn into_server_erased(
        self: Box<Self>,
    ) -> (Channel, BoxFuture<'static, Result<(), crate::Error>>) {
        (*self).into_server()
    }
}

/// A dynamically-typed component for heterogeneous collections.
///
/// This type wraps any [`Component`] implementation and provides dynamic dispatch,
/// allowing you to store different component types in the same collection.
///
/// The type parameter `L` is the link type that all components in the
/// collection can serve as transports for.
///
/// # Examples
///
/// ```rust,ignore
/// use sacp::{DynComponent, peer::AgentToClient};
///
/// let components: Vec<DynComponent<AgentToClient>> = vec![
///     DynComponent::new(Proxy1),
///     DynComponent::new(Proxy2),
///     DynComponent::new(Agent),
/// ];
/// ```
pub struct DynComponent<L: JrLink> {
    inner: Box<dyn ErasedComponent<L>>,
    _marker: PhantomData<L>,
}

impl<L: JrLink> DynComponent<L> {
    /// Create a new `DynComponent` from any type implementing [`Component`].
    pub fn new<C: Component<L>>(component: C) -> Self {
        Self {
            inner: Box::new(component),
            _marker: PhantomData,
        }
    }

    /// Returns the type name of the wrapped component.
    pub fn type_name(&self) -> String {
        self.inner.type_name()
    }
}

impl<L: JrLink> Component<L> for DynComponent<L> {
    async fn serve(self, client: impl Component<L::ConnectsTo>) -> Result<(), crate::Error> {
        self.inner
            .serve_erased(Box::new(client) as Box<dyn ErasedComponent<L::ConnectsTo>>)
            .await
    }

    fn into_server(self) -> (Channel, BoxFuture<'static, Result<(), crate::Error>>) {
        self.inner.into_server_erased()
    }
}

impl<L: JrLink> Debug for DynComponent<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynComponent")
            .field("type_name", &self.type_name())
            .finish()
    }
}

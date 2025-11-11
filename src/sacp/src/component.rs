//! Component abstraction for agents and proxies.
//!
//! This module provides the [`Component`] trait that defines the interface for things
//! that can be run as part of a conductor's chain - agents, proxies, or any ACP-speaking component.
//!
//! ## Usage
//!
//! Components are always servers that communicate via message channels with their conductor.
//! To implement a component, simply implement the trait with an async function:
//!
//! ```rust,ignore
//! use sacp::component::Component;
//! use sacp::Channels;
//!
//! struct MyProxy {
//!     // configuration fields
//! }
//!
//! impl Component for MyProxy {
//!     async fn serve(self, channels: Channels) -> Result<(), sacp::Error> {
//!         sacp::JrHandlerChain::new()
//!             .name("my-proxy")
//!             // configure handlers here
//!             .serve(channels)
//!             .await
//!     }
//! }
//! ```

use futures::future::BoxFuture;
use std::future::Future;

use crate::Channels;

/// A component that can participate in the Agent-Client Protocol.
///
/// This trait represents anything that can communicate via JSON-RPC messages over channels -
/// agents, proxies, in-process connections, or any ACP-speaking component. Components receive
/// [`Channels`] for bidirectional message passing and serve on those channels until completion.
///
/// # Component Types
///
/// The trait is implemented by several built-in types representing different communication patterns:
///
/// - **[`ByteStreams`]**: A component communicating over byte streams (stdin/stdout, sockets, etc.)
/// - **[`Channels`]**: A component communicating via in-process message channels (for testing or direct connections)
/// - **[`AcpAgent`]**: An external agent running in a separate process with stdio communication
/// - **Custom components**: Proxies, transformers, or any ACP-aware service
///
/// # Implementation
///
/// Implement this trait using async fn syntax. Most components set up a [`JrHandlerChain`]
/// to handle incoming messages:
///
/// ```rust,ignore
/// use sacp::Component;
/// use sacp::Channels;
///
/// struct MyProxy {
///     config: ProxyConfig,
/// }
///
/// impl Component for MyProxy {
///     async fn serve(self, channels: Channels) -> Result<(), sacp::Error> {
///         sacp::JrHandlerChain::new()
///             .name("my-proxy")
///             .on_receive_request(async |req: MyRequest, cx| {
///                 // Transform and forward request
///                 cx.respond(MyResponse { status: "ok".into() })
///             })
///             .serve(channels)
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
/// let components: Vec<DynComponent> = vec![
///     DynComponent::new(proxy1),
///     DynComponent::new(proxy2),
///     DynComponent::new(agent),
/// ];
/// ```
///
/// [`ByteStreams`]: crate::ByteStreams
/// [`AcpAgent`]: https://docs.rs/sacp-tokio/latest/sacp_tokio/struct.AcpAgent.html
/// [`JrHandlerChain`]: crate::JrHandlerChain
pub trait Component: Send + 'static {
    /// Serve this component on the given channels.
    ///
    /// The component should set up its handler chain and serve on the provided
    /// channels, which connect it to its conductor.
    ///
    /// # Arguments
    ///
    /// * `channels` - Bidirectional message channels for communicating with the conductor
    ///
    /// # Returns
    ///
    /// A future that resolves when the component stops serving, either successfully
    /// or with an error. The future must be `Send`.
    fn serve(self, channels: Channels) -> impl Future<Output = Result<(), crate::Error>> + Send;
}

/// Type-erased component trait for object-safe dynamic dispatch.
///
/// This trait is internal and used by [`DynComponent`]. Users should implement
/// [`Component`] instead, which is automatically converted to `ErasedComponent`
/// via a blanket implementation.
trait ErasedComponent: Send {
    fn serve_erased(
        self: Box<Self>,
        channels: Channels,
    ) -> BoxFuture<'static, Result<(), crate::Error>>;
}

/// Blanket implementation: any `Component` can be type-erased.
impl<C: Component> ErasedComponent for C {
    fn serve_erased(
        self: Box<Self>,
        channels: Channels,
    ) -> BoxFuture<'static, Result<(), crate::Error>> {
        Box::pin(async move { (*self).serve(channels).await })
    }
}

/// A dynamically-typed component for heterogeneous collections.
///
/// This type wraps any [`Component`] implementation and provides dynamic dispatch,
/// allowing you to store different component types in the same collection.
///
/// # Examples
///
/// ```rust,ignore
/// use sacp::DynComponent;
///
/// let components: Vec<DynComponent> = vec![
///     DynComponent::new(Proxy1),
///     DynComponent::new(Proxy2),
///     DynComponent::new(Agent),
/// ];
/// ```
pub struct DynComponent {
    inner: Box<dyn ErasedComponent>,
}

impl DynComponent {
    /// Create a new `DynComponent` from any type implementing [`Component`].
    pub fn new<C: Component>(component: C) -> Self {
        Self {
            inner: Box::new(component),
        }
    }
}

impl Component for DynComponent {
    async fn serve(self, channels: Channels) -> Result<(), crate::Error> {
        self.inner.serve_erased(channels).await
    }
}

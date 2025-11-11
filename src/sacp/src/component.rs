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

use crate::{Channels, Transport};

/// A component that can be run as part of a conductor's chain.
///
/// Components are always servers that receive [`Channels`] for bidirectional
/// communication with their conductor. The channels carry JSON-RPC messages
/// between the conductor and component.
///
/// # Implementation
///
/// Implement this trait using async fn syntax:
///
/// ```rust,ignore
/// use sacp::Component;
/// use sacp::Channels;
///
/// struct MyComponent;
///
/// impl Component for MyComponent {
///     async fn serve(self, channels: Channels) -> Result<(), sacp::Error> {
///         sacp::JrHandlerChain::new()
///             .name("my-component")
///             .on_receive_request(async |req: MyRequest, cx| {
///                 cx.respond(MyResponse { status: "ok".into() })
///             })
///             .serve(channels)
///             .await
///     }
/// }
/// ```
///
/// For heterogeneous collections, use [`DynComponent`]:
///
/// ```rust,ignore
/// let components: Vec<DynComponent> = vec![
///     DynComponent::new(Component1),
///     DynComponent::new(Component2),
/// ];
/// ```
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

/// Blanket implementation: any `Component` can be used as a `Transport`.
///
/// This enables using components in any context that expects a transport.
/// The component is automatically boxed and type-erased when used as a transport.
impl<C: Component> Transport for C {
    fn transport(
        self: Box<Self>,
        channels: Channels,
    ) -> BoxFuture<'static, Result<(), crate::Error>> {
        Box::pin(async move { (*self).serve(channels).await })
    }
}

//! Component abstraction for agents and proxies.
//!
//! This module provides the [`Component`] trait that defines the interface for things
//! that can be run as part of a conductor's chain - agents, proxies, or any ACP-speaking component.
//!
//! ## Usage
//!
//! Components are always servers that communicate via message channels with their conductor.
//! To implement a component, create a handler chain and serve it on the provided channels:
//!
//! ```rust,ignore
//! use sacp::component::Component;
//! use sacp::{Channels, JrHandlerChain};
//! use futures::future::BoxFuture;
//!
//! struct MyProxy {
//!     // configuration fields
//! }
//!
//! impl Component for MyProxy {
//!     fn run(self: Box<Self>, channels: Channels) -> BoxFuture<'static, Result<(), sacp::Error>> {
//!         Box::pin(async move {
//!             JrHandlerChain::new()
//!                 .name("my-proxy")
//!                 // configure handlers here
//!                 .serve(channels)
//!                 .await
//!         })
//!     }
//! }
//! ```

use futures::future::BoxFuture;

use crate::{Channels, Transport};

/// A component that can be run as part of a conductor's chain.
///
/// Components are always servers that receive [`Channels`] for bidirectional
/// communication with their conductor. The channels carry JSON-RPC messages
/// between the conductor and component.
///
/// # Implementation
///
/// Most implementations will:
/// 1. Create a [`JrHandlerChain`](crate::JrHandlerChain) with the desired handlers
/// 2. Call `.serve(channels)` on it
/// 3. Return the resulting future wrapped in a `BoxFuture`
///
/// # Examples
///
/// Basic component implementation:
///
/// ```rust,ignore
/// use sacp::component::Component;
/// use sacp::{Channels, JrHandlerChain};
/// use futures::future::BoxFuture;
///
/// struct MyComponent;
///
/// impl Component for MyComponent {
///     fn serve(self: Box<Self>, channels: Channels) -> BoxFuture<'static, Result<(), sacp::Error>> {
///         Box::pin(async move {
///             JrHandlerChain::new()
///                 .name("my-component")
///                 .on_receive_request(async |req: MyRequest, cx| {
///                     cx.respond(MyResponse { status: "ok".into() })
///                 })
///                 .serve(channels)
///                 .await
///         })
///     }
/// }
/// ```
pub trait Component: Send {
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
    /// or with an error.
    fn serve(self: Box<Self>, channels: Channels) -> BoxFuture<'static, Result<(), crate::Error>>;
}

/// Blanket implementation: any `Transport` can be used as a `Component`.
///
/// Since both `Channels` and `Channels` now have the same signature
/// (both use `Result<Message, Error>` for incoming), we can directly pass through.
impl<T: Transport + 'static> Component for T {
    fn serve(self: Box<Self>, channels: Channels) -> BoxFuture<'static, Result<(), crate::Error>> {
        Box::pin(async move {
            // Both Channels and Channels now have identical types,
            // so we can convert directly
            let transport_channels =
                Channels::new(channels.remote_outgoing_rx, channels.remote_incoming_tx);
            self.transport(transport_channels).await
        })
    }
}

/// Blanket implementation: any `Component` can be used as a `Transport`.
///
/// Since both `Channels` and `Channels` now have the same signature
/// (both use `Result<Message, Error>` for incoming), we can directly pass through.
impl Transport for dyn Component {
    fn transport(
        self: Box<Self>,
        channels: Channels,
    ) -> BoxFuture<'static, Result<(), crate::Error>> {
        Box::pin(async move {
            // Both Channels and Channels now have identical types,
            // so we can convert directly
            let channels = Channels::new(channels.remote_outgoing_rx, channels.remote_incoming_tx);
            self.serve(channels).await
        })
    }
}

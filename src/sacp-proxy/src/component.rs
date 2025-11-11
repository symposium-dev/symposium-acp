//! Component trait for proxy chain composition.
//!
//! The [`Component`] trait defines the interface for things that can be part of
//! a proxy chain - proxies, agents, or any other ACP-speaking component.

use futures::channel::mpsc;
use sacp::{IntoJrTransport, JrConnectionCx};

/// A component that can be part of a proxy chain.
///
/// This trait represents anything that can be spawned and connected in a proxy
/// chain - typically proxies or agents that communicate via ACP.
///
/// The trait provides an explicit interface for setting up bidirectional
/// communication between components, making the message flow directionality
/// clear through channel naming.
///
/// # Channel Directionality
///
/// The `connect_component()` method establishes bidirectional communication:
/// - **`predecessor_to_component_rx`**: Messages flowing from predecessor to this component
/// - **`component_to_predecessor_tx`**: Messages flowing from this component back to predecessor
///
/// The component is responsible for bridging these channels with its internal
/// transport mechanism (stdio, network, etc.).
///
/// # Examples
///
/// Most types that implement `IntoJrTransport` automatically implement `Component`:
///
/// ```ignore
/// use sacp_tokio::AcpAgent;
/// use sacp_proxy::Component;
///
/// let component: Box<dyn Component> = Box::new(AcpAgent::from_str("python proxy.py")?);
/// // Component can be passed directly to connect_to() via IntoJrTransport impl
/// ```
pub trait Component: Send {
    /// Connects the component by bridging predecessor channels with the component's transport.
    ///
    /// # Arguments
    ///
    /// * `cx` - The connection context for spawning tasks
    /// * `predecessor_to_component_rx` - Channel for messages from predecessor to component
    /// * `component_to_predecessor_tx` - Channel for messages from component back to predecessor
    ///
    /// # Returns
    ///
    /// `Ok(())` if the connection was successfully established, or an error if setup failed.
    fn connect_component(
        self: Box<Self>,
        cx: &JrConnectionCx,
        predecessor_to_component_rx: mpsc::UnboundedReceiver<sacp::jsonrpcmsg::Message>,
        component_to_predecessor_tx: mpsc::UnboundedSender<sacp::jsonrpcmsg::Message>,
    ) -> Result<(), sacp::Error>;
}

/// Blanket implementation: any `IntoJrTransport` can be used as a `Component`.
///
/// This bridges from the explicit channel interface to the `IntoJrTransport`
/// interface by mapping channel directions appropriately.
impl<T: IntoJrTransport + 'static> Component for T {
    fn connect_component(
        self: Box<Self>,
        cx: &JrConnectionCx,
        predecessor_to_component_rx: mpsc::UnboundedReceiver<sacp::jsonrpcmsg::Message>,
        component_to_predecessor_tx: mpsc::UnboundedSender<sacp::jsonrpcmsg::Message>,
    ) -> Result<(), sacp::Error> {
        // From component's perspective:
        // - predecessor_to_component_rx is incoming messages
        // - component_to_predecessor_tx is outgoing messages
        self.into_jr_transport(cx, predecessor_to_component_rx, component_to_predecessor_tx)
    }
}

/// Bridge from `Component` back to `IntoJrTransport` for convenience.
///
/// This allows `Box<dyn Component>` to be used with `JrHandlerChain::connect_to()`.
impl IntoJrTransport for dyn Component {
    fn into_jr_transport(
        self: Box<Self>,
        cx: &JrConnectionCx,
        outgoing_rx: mpsc::UnboundedReceiver<sacp::jsonrpcmsg::Message>,
        incoming_tx: mpsc::UnboundedSender<sacp::jsonrpcmsg::Message>,
    ) -> Result<(), sacp::Error> {
        // From predecessor's perspective:
        // - outgoing_rx is messages to send to component (predecessor → component)
        // - incoming_tx is where component sends messages back (component → predecessor)
        self.connect_component(cx, outgoing_rx, incoming_tx)
    }
}

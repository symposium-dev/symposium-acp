use crate::schema::SessionNotification;
use serde::Serialize;

use crate::jsonrpc::{JsonRpcMessage, JsonRpcNotification};

const METHOD_SESSION_UPDATE: &str = "session/update";

// Agent -> Client notifications
// These are one-way messages that agents send to clients/editors

impl JsonRpcMessage for SessionNotification {
    fn matches_method(method: &str) -> bool {
        method == METHOD_SESSION_UPDATE
    }

    fn method(&self) -> &str {
        METHOD_SESSION_UPDATE
    }

    fn to_untyped_message(&self) -> Result<crate::UntypedMessage, crate::Error> {
        crate::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Result<Self, crate::Error> {
        if !Self::matches_method(method) {
            return Err(crate::Error::method_not_found());
        }
        crate::util::json_cast(params)
    }
}

impl JsonRpcNotification for SessionNotification {}

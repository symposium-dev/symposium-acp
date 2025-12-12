use crate::schema::SessionNotification;
use serde::Serialize;

use crate::jsonrpc::{JrMessage, JrNotification};

// Agent -> Client notifications
// These are one-way messages that agents send to clients/editors

impl JrMessage for SessionNotification {
    fn method(&self) -> &str {
        "session/update"
    }

    fn to_untyped_message(&self) -> Result<crate::UntypedMessage, crate::Error> {
        crate::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Option<Result<Self, crate::Error>> {
        if method != "session/update" {
            return None;
        }
        Some(crate::util::json_cast(params))
    }
}

impl JrNotification for SessionNotification {}

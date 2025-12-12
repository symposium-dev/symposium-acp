use crate::schema::CancelNotification;
use serde::Serialize;

use crate::jsonrpc::{JrMessage, JrNotification};
use crate::util::json_cast;

impl JrMessage for CancelNotification {
    fn method(&self) -> &str {
        "session/cancel"
    }

    fn to_untyped_message(&self) -> Result<crate::UntypedMessage, crate::Error> {
        crate::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Option<Result<Self, crate::Error>> {
        if method != "session/cancel" {
            return None;
        }
        Some(json_cast(params))
    }
}

impl JrNotification for CancelNotification {}

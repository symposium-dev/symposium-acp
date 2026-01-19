use crate::schema::CancelNotification;
use serde::Serialize;

use crate::jsonrpc::{JrMessage, JrNotification};
use crate::util::json_cast;

const METHOD_SESSION_CANCEL: &str = "session/cancel";

impl JrMessage for CancelNotification {
    fn matches_method(method: &str) -> bool {
        method == METHOD_SESSION_CANCEL
    }

    fn method(&self) -> &str {
        METHOD_SESSION_CANCEL
    }

    fn to_untyped_message(&self) -> Result<crate::UntypedMessage, crate::Error> {
        crate::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Result<Self, crate::Error> {
        if !Self::matches_method(method) {
            return Err(crate::Error::method_not_found());
        }
        json_cast(params)
    }
}

impl JrNotification for CancelNotification {}

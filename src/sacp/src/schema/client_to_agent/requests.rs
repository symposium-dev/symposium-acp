use crate::schema::{
    AuthenticateRequest, AuthenticateResponse, InitializeRequest, InitializeResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, SetSessionModeRequest, SetSessionModeResponse,
};
use serde::Serialize;

use crate::jsonrpc::{JrMessage, JrRequest, JrResponsePayload};
use crate::util::json_cast;

// Method constants
const METHOD_INITIALIZE: &str = "initialize";
const METHOD_AUTHENTICATE: &str = "authenticate";
const METHOD_SESSION_LOAD: &str = "session/load";
const METHOD_SESSION_NEW: &str = "session/new";
const METHOD_SESSION_PROMPT: &str = "session/prompt";
const METHOD_SESSION_SET_MODE: &str = "session/set_mode";

// ============================================================================
// InitializeRequest
// ============================================================================

impl JrMessage for InitializeRequest {
    fn matches_method(method: &str) -> bool {
        method == METHOD_INITIALIZE
    }

    fn method(&self) -> &str {
        METHOD_INITIALIZE
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

impl JrRequest for InitializeRequest {
    type Response = InitializeResponse;
}

impl JrResponsePayload for InitializeResponse {
    fn into_json(self, _method: &str) -> Result<serde_json::Value, crate::Error> {
        serde_json::to_value(self).map_err(crate::Error::into_internal_error)
    }

    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, crate::Error> {
        json_cast(&value)
    }
}

// ============================================================================
// AuthenticateRequest
// ============================================================================

impl JrMessage for AuthenticateRequest {
    fn matches_method(method: &str) -> bool {
        method == METHOD_AUTHENTICATE
    }

    fn method(&self) -> &str {
        METHOD_AUTHENTICATE
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

impl JrRequest for AuthenticateRequest {
    type Response = AuthenticateResponse;
}

impl JrResponsePayload for AuthenticateResponse {
    fn into_json(self, _method: &str) -> Result<serde_json::Value, crate::Error> {
        serde_json::to_value(self).map_err(crate::Error::into_internal_error)
    }

    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, crate::Error> {
        json_cast(&value)
    }
}

// ============================================================================
// LoadSessionRequest
// ============================================================================

impl JrMessage for LoadSessionRequest {
    fn matches_method(method: &str) -> bool {
        method == METHOD_SESSION_LOAD
    }

    fn method(&self) -> &str {
        METHOD_SESSION_LOAD
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

impl JrRequest for LoadSessionRequest {
    type Response = LoadSessionResponse;
}

impl JrResponsePayload for LoadSessionResponse {
    fn into_json(self, _method: &str) -> Result<serde_json::Value, crate::Error> {
        serde_json::to_value(self).map_err(crate::Error::into_internal_error)
    }

    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, crate::Error> {
        json_cast(&value)
    }
}

// ============================================================================
// NewSessionRequest
// ============================================================================

impl JrMessage for NewSessionRequest {
    fn matches_method(method: &str) -> bool {
        method == METHOD_SESSION_NEW
    }

    fn method(&self) -> &str {
        METHOD_SESSION_NEW
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

impl JrRequest for NewSessionRequest {
    type Response = NewSessionResponse;
}

impl JrResponsePayload for NewSessionResponse {
    fn into_json(self, _method: &str) -> Result<serde_json::Value, crate::Error> {
        serde_json::to_value(self).map_err(crate::Error::into_internal_error)
    }

    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, crate::Error> {
        json_cast(&value)
    }
}

// ============================================================================
// PromptRequest
// ============================================================================

impl JrMessage for PromptRequest {
    fn matches_method(method: &str) -> bool {
        method == METHOD_SESSION_PROMPT
    }

    fn method(&self) -> &str {
        METHOD_SESSION_PROMPT
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

impl JrRequest for PromptRequest {
    type Response = PromptResponse;
}

impl JrResponsePayload for PromptResponse {
    fn into_json(self, _method: &str) -> Result<serde_json::Value, crate::Error> {
        serde_json::to_value(self).map_err(crate::Error::into_internal_error)
    }

    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, crate::Error> {
        json_cast(&value)
    }
}

// ============================================================================
// SetSessionModeRequest
// ============================================================================

impl JrMessage for SetSessionModeRequest {
    fn matches_method(method: &str) -> bool {
        method == METHOD_SESSION_SET_MODE
    }

    fn method(&self) -> &str {
        METHOD_SESSION_SET_MODE
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

impl JrRequest for SetSessionModeRequest {
    type Response = SetSessionModeResponse;
}

impl JrResponsePayload for SetSessionModeResponse {
    fn into_json(self, _method: &str) -> Result<serde_json::Value, crate::Error> {
        serde_json::to_value(self).map_err(crate::Error::into_internal_error)
    }

    fn from_value(_method: &str, value: serde_json::Value) -> Result<Self, crate::Error> {
        json_cast(&value)
    }
}

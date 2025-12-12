use crate::schema::{
    AuthenticateRequest, AuthenticateResponse, InitializeRequest, InitializeResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, SetSessionModeRequest, SetSessionModeResponse,
};
use serde::Serialize;

use crate::jsonrpc::{JrMessage, JrRequest, JrResponsePayload};
use crate::util::json_cast;

// ============================================================================
// InitializeRequest
// ============================================================================

impl JrMessage for InitializeRequest {
    fn method(&self) -> &str {
        "initialize"
    }

    fn to_untyped_message(&self) -> Result<crate::UntypedMessage, crate::Error> {
        crate::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Option<Result<Self, crate::Error>> {
        if method != "initialize" {
            return None;
        }
        Some(json_cast(params))
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
    fn method(&self) -> &str {
        "authenticate"
    }

    fn to_untyped_message(&self) -> Result<crate::UntypedMessage, crate::Error> {
        crate::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Option<Result<Self, crate::Error>> {
        if method != "authenticate" {
            return None;
        }
        Some(json_cast(params))
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
    fn method(&self) -> &str {
        "session/load"
    }

    fn to_untyped_message(&self) -> Result<crate::UntypedMessage, crate::Error> {
        crate::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Option<Result<Self, crate::Error>> {
        if method != "session/load" {
            return None;
        }
        Some(json_cast(params))
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
    fn method(&self) -> &str {
        "session/new"
    }

    fn to_untyped_message(&self) -> Result<crate::UntypedMessage, crate::Error> {
        crate::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Option<Result<Self, crate::Error>> {
        if method != "session/new" {
            return None;
        }
        Some(json_cast(params))
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
    fn method(&self) -> &str {
        "session/prompt"
    }

    fn to_untyped_message(&self) -> Result<crate::UntypedMessage, crate::Error> {
        crate::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Option<Result<Self, crate::Error>> {
        if method != "session/prompt" {
            return None;
        }
        Some(json_cast(params))
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
    fn method(&self) -> &str {
        "session/set_mode"
    }

    fn to_untyped_message(&self) -> Result<crate::UntypedMessage, crate::Error> {
        crate::UntypedMessage::new(self.method(), self)
    }

    fn parse_message(method: &str, params: &impl Serialize) -> Option<Result<Self, crate::Error>> {
        if method != "session/set_mode" {
            return None;
        }
        Some(json_cast(params))
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

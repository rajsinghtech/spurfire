//! Typed, secret-safe HTTP errors.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use spurfire_protocol::{ApiErrorResponse, ApiValidationError, ResponseMetadata};

/// HTTP status paired with the stable protocol error body.
#[derive(Clone, Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ApiErrorResponse,
}

impl ApiError {
    /// Creates a real-mode error with no state reason.
    #[must_use]
    pub fn new(status: StatusCode, code: &str, message: &str) -> Self {
        Self {
            status,
            body: ApiErrorResponse {
                code: code.to_owned(),
                message: message.to_owned(),
                state_reason: None,
                metadata: ResponseMetadata::default(),
            },
        }
    }

    /// Marks the response as simulated without exposing provider details.
    #[must_use]
    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.body.metadata.dry_run = dry_run;
        self
    }

    /// Adds a stable machine-readable lobby state reason.
    #[must_use]
    pub fn state_reason(mut self, reason: impl Into<String>) -> Self {
        self.body.state_reason = Some(reason.into());
        self
    }

    /// Maps protocol validation to stable status and code values.
    #[must_use]
    pub fn validation(error: &ApiValidationError, dry_run: bool) -> Self {
        let (status, code, message) = match error {
            ApiValidationError::WireVersionIncompatible(_) => (
                StatusCode::CONFLICT,
                "wire_version_incompatible",
                "client and service wire major versions are incompatible",
            ),
            ApiValidationError::InvalidConnectivitySample(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_measurements",
                "measurement values do not describe the current roster",
            ),
            ApiValidationError::MixedAuthorityFormula { .. } => (
                StatusCode::CONFLICT,
                "authority_formula_incompatible",
                "roster contains incompatible authority formula versions",
            ),
            ApiValidationError::RosterWireVersionIncompatible { .. } => (
                StatusCode::CONFLICT,
                "wire_version_incompatible",
                "roster contains incompatible wire major versions",
            ),
            ApiValidationError::SecureSessionWireVersionRequired { .. } => (
                StatusCode::CONFLICT,
                "session_identity_required",
                "every real-lobby roster member must support signed wire 1.2 sessions",
            ),
            ApiValidationError::EmptyDisplayName
            | ApiValidationError::DisplayNameTooLong
            | ApiValidationError::InvalidMaxPlayers { .. } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                "request fields failed validation",
            ),
        };
        Self::new(status, code, message).dry_run(dry_run)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(self.body)).into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
        headers.insert(header::VARY, HeaderValue::from_static("Authorization"));
        headers.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        response
    }
}

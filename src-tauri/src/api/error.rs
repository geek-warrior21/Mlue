use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiErrorDetails {
    pub message: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiErrorResponse {
    pub error: OpenAiErrorDetails,
}

impl OpenAiErrorResponse {
    pub fn new(
        message: impl Into<String>,
        err_type: impl Into<String>,
        param: Option<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            error: OpenAiErrorDetails {
                message: message.into(),
                r#type: err_type.into(),
                param,
                code: Some(code.into()),
            },
        }
    }
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub response: OpenAiErrorResponse,
}

impl ApiError {
    pub fn unauthorized(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            response: OpenAiErrorResponse::new(message, "invalid_request_error", None, code),
        }
    }

    pub fn forbidden(
        message: impl Into<String>,
        code: impl Into<String>,
        param: Option<String>,
    ) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            response: OpenAiErrorResponse::new(message, "permission_error", param, code),
        }
    }

    pub fn not_found(
        message: impl Into<String>,
        code: impl Into<String>,
        param: Option<String>,
    ) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            response: OpenAiErrorResponse::new(message, "invalid_request_error", param, code),
        }
    }

    pub fn bad_request(
        message: impl Into<String>,
        code: impl Into<String>,
        param: Option<String>,
    ) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            response: OpenAiErrorResponse::new(message, "invalid_request_error", param, code),
        }
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            response: OpenAiErrorResponse::new(
                message,
                "invalid_request_error",
                None,
                "request_too_large",
            ),
        }
    }

    pub fn service_unavailable(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            response: OpenAiErrorResponse::new(message, "api_error", None, code),
        }
    }

    pub fn gateway_timeout(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GATEWAY_TIMEOUT,
            response: OpenAiErrorResponse::new(message, "api_error", None, "timeout"),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            response: OpenAiErrorResponse::new(message, "api_error", None, "internal_error"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.response)).into_response()
    }
}

pub struct AppJson<T>(pub T);

impl<S, T> axum::extract::FromRequest<S> for AppJson<T>
where
    axum::Json<T>:
        axum::extract::FromRequest<S, Rejection = axum::extract::rejection::JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(value) => Ok(AppJson(value.0)),
            Err(rejection) => {
                let status = rejection.status();
                if status == StatusCode::PAYLOAD_TOO_LARGE {
                    Err(ApiError::payload_too_large(
                        "Request payload exceeds size limit",
                    ))
                } else {
                    Err(ApiError::bad_request(
                        format!("Failed to parse request JSON: {}", rejection.body_text()),
                        "invalid_json",
                        None,
                    ))
                }
            }
        }
    }
}

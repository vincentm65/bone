use bone::llm::{LlmError, LlmErrorKind};
use bone::ui::app::stream::{StreamFailure, timeout_message};

#[test]
fn timeout_and_connection_failures_are_retryable() {
    assert!(StreamFailure::InitialTimeout.retryable());
    assert!(StreamFailure::IdleTimeout.retryable());
    assert!(
        StreamFailure::Provider(LlmError::new_with_kind(
            LlmErrorKind::Timeout,
            "request timed out",
        ))
        .retryable()
    );
    assert!(
        StreamFailure::Provider(LlmError::new_with_kind(
            LlmErrorKind::Connection,
            "connection refused",
        ))
        .retryable()
    );
}

#[test]
fn non_retryable_provider_failures() {
    for kind in [
        LlmErrorKind::Auth,
        LlmErrorKind::RateLimit,
        LlmErrorKind::Server(500),
        LlmErrorKind::Parse,
        LlmErrorKind::Config,
    ] {
        assert!(
            !StreamFailure::Provider(LlmError::new_with_kind(kind, "provider failed")).retryable()
        );
    }
}

#[test]
fn final_timeout_messages_include_retry_status() {
    assert_eq!(
        timeout_message("provider timeout", "no response", true),
        "[provider timeout: no response within 90s; retried once]",
    );
    assert_eq!(
        timeout_message("stream timeout", "no events", true),
        "[stream timeout: no events within 90s; retried once]",
    );
}

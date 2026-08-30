use reqwest::{header::HeaderMap, StatusCode};
use std::time::Duration;

pub const MAX_PROVIDER_ATTEMPTS: usize = 3;

pub fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::BAD_GATEWAY
        || status == StatusCode::SERVICE_UNAVAILABLE
        || status == StatusCode::GATEWAY_TIMEOUT
}

pub fn retry_delay(headers: &HeaderMap, attempt: usize) -> Duration {
    if let Some(value) = headers.get(reqwest::header::RETRY_AFTER) {
        if let Ok(value) = value.to_str() {
            if let Ok(seconds) = value.trim().parse::<u64>() {
                return Duration::from_secs(seconds.clamp(1, 30));
            }
        }
    }
    Duration::from_millis(500_u64.saturating_mul(1_u64 << attempt.min(5)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

    #[test]
    fn retries_transient_provider_statuses_only() {
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn honors_retry_after_with_bound() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("120"));
        assert_eq!(retry_delay(&headers, 0), Duration::from_secs(30));
    }
}

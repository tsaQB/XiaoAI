use serde_json::Value;
use std::time::Duration;

pub const MAX_TELEGRAM_ATTEMPTS: usize = 3;

fn bounded_attempt(attempt: usize) -> bool {
    attempt + 1 < MAX_TELEGRAM_ATTEMPTS
}

fn backoff(attempt: usize) -> Duration {
    Duration::from_millis(500_u64.saturating_mul(1_u64 << attempt.min(4)))
}

pub fn retry_delay_from_response(response: &Value, attempt: usize) -> Option<Duration> {
    if !bounded_attempt(attempt) {
        return None;
    }
    let code = response.get("error_code").and_then(Value::as_i64)?;
    if code == 429 {
        return response
            .pointer("/parameters/retry_after")
            .and_then(Value::as_u64)
            .map(Duration::from_secs)
            .or_else(|| Some(backoff(attempt)));
    }
    if (500..=599).contains(&code) {
        return Some(backoff(attempt));
    }
    None
}

pub fn retry_delay_from_error(error: &str, attempt: usize) -> Option<Duration> {
    if !bounded_attempt(attempt) {
        return None;
    }
    if let Some(value) = error
        .split("retry_after=")
        .nth(1)
        .and_then(|tail| tail.split('s').next())
        .and_then(|seconds| seconds.parse::<u64>().ok())
    {
        return Some(Duration::from_secs(value));
    }
    if let Some(code) = error
        .split("code=")
        .nth(1)
        .and_then(|tail| tail.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|value| value.parse::<u16>().ok())
    {
        if (500..=599).contains(&code) {
            return Some(backoff(attempt));
        }
        return None;
    }
    let lower = error.to_ascii_lowercase();
    if [
        "timeout",
        "connection failure",
        "transport failure",
        "request failure",
        "body failure",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return Some(backoff(attempt));
    }
    None
}

pub fn fallback_allowed_response(response: &Value) -> bool {
    response.get("error_code").and_then(Value::as_i64) == Some(400)
}

pub fn fallback_allowed_error(error: &str) -> bool {
    error.contains("code=400")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn honors_retry_after_for_429() {
        let response = json!({
            "ok": false,
            "error_code": 429,
            "parameters": {"retry_after": 7}
        });
        assert_eq!(
            retry_delay_from_response(&response, 0),
            Some(Duration::from_secs(7))
        );
    }

    #[test]
    fn retries_5xx_but_not_deterministic_4xx() {
        assert!(retry_delay_from_response(&json!({"error_code": 503}), 0).is_some());
        assert!(retry_delay_from_response(&json!({"error_code": 401}), 0).is_none());
        assert!(retry_delay_from_response(&json!({"error_code": 400}), 0).is_none());
    }

    #[test]
    fn retries_are_bounded() {
        assert!(retry_delay_from_response(&json!({"error_code": 503}), 1).is_some());
        assert!(retry_delay_from_response(&json!({"error_code": 503}), 2).is_none());
    }

    #[test]
    fn parses_retry_after_from_normalized_api_errors() {
        assert_eq!(
            retry_delay_from_error(
                "Telegram API error [sendMessage] code=429: Too Many Requests retry_after=9s",
                0,
            ),
            Some(Duration::from_secs(9))
        );
    }

    #[test]
    fn rich_or_media_fallback_is_only_for_bad_request() {
        assert!(fallback_allowed_response(&json!({"error_code": 400})));
        assert!(!fallback_allowed_response(&json!({"error_code": 429})));
        assert!(!fallback_allowed_response(&json!({"error_code": 503})));
        assert!(fallback_allowed_error(
            "Telegram API error [x] code=400: bad request"
        ));
        assert!(!fallback_allowed_error(
            "Telegram API error [x] code=429: rate limited"
        ));
    }
}

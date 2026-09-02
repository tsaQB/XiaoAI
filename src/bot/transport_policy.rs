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
    if let Some(code) = normalized_api_error_code(error) {
        if code == 429 {
            return error
                .rsplit_once(" retry_after=")
                .and_then(|(_, tail)| tail.strip_suffix('s'))
                .and_then(|seconds| seconds.parse::<u64>().ok())
                .map(Duration::from_secs)
                .or_else(|| Some(backoff(attempt)));
        }
        if (500..=599).contains(&code) {
            return Some(backoff(attempt));
        }
        return None;
    }
    if error.to_ascii_lowercase().contains("connection failure") {
        return Some(backoff(attempt));
    }
    None
}

fn normalized_api_error_code(error: &str) -> Option<u16> {
    let error = error.strip_prefix("Telegram API error [")?;
    let (_, tail) = error.split_once("] code=")?;
    let (code, _) = tail.split_once(':')?;
    code.parse().ok()
}

pub fn fallback_allowed_response(response: &Value) -> bool {
    response.get("error_code").and_then(Value::as_i64) == Some(400)
}

pub fn fallback_allowed_error(error: &str) -> bool {
    normalized_api_error_code(error) == Some(400)
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

    #[test]
    fn normalized_error_policy_ignores_description_substrings() {
        assert!(!fallback_allowed_error(
            "Telegram API error [x] code=500: upstream code=400"
        ));
        assert!(!fallback_allowed_error(
            "Telegram API error [x] code=4000: unknown"
        ));
        assert!(
            retry_delay_from_error("Telegram API error [x] code=400: bad retry_after=9s", 0)
                .is_none()
        );
    }

    #[test]
    fn retries_only_transport_failures_known_to_precede_request_delivery() {
        assert!(retry_delay_from_error("connection failure", 0).is_some());
        assert!(retry_delay_from_error("timeout", 0).is_none());
        assert!(retry_delay_from_error("body failure", 0).is_none());
        assert!(retry_delay_from_error("transport failure", 0).is_none());
        assert!(retry_delay_from_error("request failure", 0).is_none());
    }
}

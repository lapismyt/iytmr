use std::{
    future::{Future, IntoFuture},
    time::Duration,
};

use anyhow::Error;
use teloxide::RequestError;

const NETWORK_MAX_ATTEMPTS: usize = 10;
const RETRY_DELAYS: [Duration; NETWORK_MAX_ATTEMPTS - 1] = [
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(20),
    Duration::from_secs(40),
    Duration::from_secs(80),
    Duration::from_secs(160),
    Duration::from_secs(320),
    Duration::from_secs(640),
    Duration::from_secs(1280),
];

pub(crate) fn format_request_error(error: &RequestError) -> String {
    match error {
        RequestError::Api(api_error) => format!("Telegram API error: {api_error}"),
        RequestError::MigrateToChatId(chat_id) => {
            format!("Telegram requires migrating to chat ID {chat_id}")
        }
        RequestError::RetryAfter(seconds) => {
            format!(
                "Telegram rate limit: retry after {} seconds",
                seconds.seconds()
            )
        }
        RequestError::Network(error) => {
            let reason = if error.is_timeout() {
                "request timed out"
            } else if error.is_connect() {
                "connection failed"
            } else if error.is_request() {
                "request could not be sent"
            } else if error.is_body() {
                "request body could not be read"
            } else if error.is_decode() {
                "response could not be decoded"
            } else {
                "transport failed"
            };

            format!("Telegram network error: {reason}")
        }
        RequestError::InvalidJson { source, .. } => {
            format!("Telegram returned invalid JSON: {source}")
        }
        RequestError::Io(error) => format!("Telegram file I/O error: {error}"),
    }
}

pub(crate) fn log_request_error(context: &str, error: &RequestError) {
    log::error!("{context}: {}", format_request_error(error));
}

pub(crate) fn log_error(context: &str, error: &Error) {
    if let Some(request_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RequestError>())
    {
        log_request_error(context, request_error);
    } else {
        log::error!("{context}: {error:#}");
    }
}

pub(crate) async fn retry_network<T, F, Operation, S, Sleep>(
    operation_name: &str,
    mut operation: F,
    mut sleep: S,
) -> Result<T, RequestError>
where
    F: FnMut() -> Operation,
    Operation: IntoFuture<Output = Result<T, RequestError>>,
    Operation::IntoFuture: Future<Output = Result<T, RequestError>>,
    S: FnMut(Duration) -> Sleep,
    Sleep: Future<Output = ()>,
{
    for (retry_index, delay) in RETRY_DELAYS.iter().copied().enumerate() {
        match operation().into_future().await {
            Ok(value) => return Ok(value),
            Err(error @ RequestError::Network(_)) => {
                log::warn!(
                    "{operation_name} failed on attempt {}/{}: {}; retrying in {} seconds",
                    retry_index + 1,
                    NETWORK_MAX_ATTEMPTS,
                    format_request_error(&error),
                    delay.as_secs(),
                );
                sleep(delay).await;
            }
            Err(error) => return Err(error),
        }
    }

    let result = operation().into_future().await;
    if let Err(error) = &result {
        log_request_error(
            &format!("{operation_name} failed after {NETWORK_MAX_ATTEMPTS} attempts"),
            error,
        );
    }

    result
}

pub(crate) async fn retry_network_with_backoff<T, F, Operation>(
    operation_name: &str,
    operation: F,
) -> Result<T, RequestError>
where
    F: FnMut() -> Operation,
    Operation: IntoFuture<Output = Result<T, RequestError>>,
    Operation::IntoFuture: Future<Output = Result<T, RequestError>>,
{
    retry_network(operation_name, operation, tokio::time::sleep).await
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use futures::future::ready;
    use teloxide::{ApiError, Bot, RequestError, prelude::Requester};

    use super::{format_request_error, retry_network};

    async fn network_error() -> RequestError {
        let token = "123456:abcdefghijklmnopqrstuvwxyzABCDEFGH_123";
        Bot::new(token)
            .set_api_url(url::Url::parse("unsupported://localhost").unwrap())
            .get_me()
            .await
            .expect_err("unsupported URL scheme must fail before making a request")
    }

    #[tokio::test]
    async fn retry_network_succeeds_on_first_attempt() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = attempts.clone();

        let result = retry_network(
            "test operation",
            move || {
                operation_attempts.fetch_add(1, Ordering::SeqCst);
                ready(Ok::<_, RequestError>("done"))
            },
            |_| ready(()),
        )
        .await;

        assert_eq!(result.unwrap(), "done");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_network_retries_then_succeeds() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = attempts.clone();
        let temporary_error = network_error().await;
        let delays = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_delays = delays.clone();

        let result = retry_network(
            "test operation",
            move || {
                let attempt = operation_attempts.fetch_add(1, Ordering::SeqCst);
                let result = if attempt == 0 {
                    Err(temporary_error.clone())
                } else {
                    Ok("done")
                };
                ready(result)
            },
            move |delay| {
                recorded_delays.lock().unwrap().push(delay);
                ready(())
            },
        )
        .await;

        assert_eq!(result.unwrap(), "done");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(*delays.lock().unwrap(), vec![Duration::from_secs(5)]);
    }

    #[tokio::test]
    async fn retry_network_stops_after_ten_network_errors() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = attempts.clone();
        let temporary_error = network_error().await;
        let delays = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_delays = delays.clone();

        let result = retry_network(
            "test operation",
            move || {
                operation_attempts.fetch_add(1, Ordering::SeqCst);
                ready::<Result<(), RequestError>>(Err(temporary_error.clone()))
            },
            move |delay| {
                recorded_delays.lock().unwrap().push(delay);
                ready(())
            },
        )
        .await;

        assert!(matches!(result, Err(RequestError::Network(_))));
        assert_eq!(attempts.load(Ordering::SeqCst), 10);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![
                Duration::from_secs(5),
                Duration::from_secs(10),
                Duration::from_secs(20),
                Duration::from_secs(40),
                Duration::from_secs(80),
                Duration::from_secs(160),
                Duration::from_secs(320),
                Duration::from_secs(640),
                Duration::from_secs(1280),
            ]
        );
    }

    #[tokio::test]
    async fn retry_network_does_not_retry_api_errors() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = attempts.clone();

        let result = retry_network(
            "test operation",
            move || {
                operation_attempts.fetch_add(1, Ordering::SeqCst);
                ready::<Result<(), RequestError>>(Err(RequestError::Api(ApiError::BotBlocked)))
            },
            |_| ready(()),
        )
        .await;

        assert!(matches!(
            result,
            Err(RequestError::Api(ApiError::BotBlocked))
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn request_error_formatting_includes_api_details() {
        let error = format_request_error(&RequestError::Api(ApiError::BotBlocked));

        assert_eq!(
            error,
            "Telegram API error: Forbidden: bot was blocked by the user"
        );
    }

    #[tokio::test]
    async fn network_error_formatting_hides_bot_token() {
        let token = "123456:abcdefghijklmnopqrstuvwxyzABCDEFGH_123";
        let error = format_request_error(&network_error().await);

        assert!(error.starts_with("Telegram network error:"));
        assert!(!error.contains(token));
        assert!(!error.contains("127.0.0.1"));
    }
}

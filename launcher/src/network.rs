//! Bounded, credential-safe GET retries. OAuth POSTs deliberately do not use these retries.
use crate::i18n::trf;
use std::{
    io::Read,
    time::Duration,
};

const ATTEMPTS: usize = 4;
const MAX_WAIT: Duration = Duration::from_secs(30);

pub(crate) fn endpoint(url: &str) -> String {
    url::Url::parse(url)
        .map(|mut u| {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        })
        .unwrap_or_else(|_| "invalid URL".into())
}

fn retry_after(value: Option<&str>) -> Option<Duration> {
    let value = value?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(Duration::from_secs(
        (date.timestamp() - chrono::Utc::now().timestamp()).max(0) as u64,
    ))
}

struct Failure {
    status: Option<u16>,
    message: String,
    retry: bool,
    after: Option<Duration>,
}

fn request(url: &str) -> Result<ureq::Response, Failure> {
    let mut request = crate::backend::agent(url)
        .get(url)
        .set("Accept-Encoding", "identity");
    if url.starts_with("https://api.github.com/") {
        if let Ok(token) = std::env::var("HORIZON_GITHUB_TOKEN") {
            if !token.is_empty() {
                request = request.set("Authorization", &format!("Bearer {token}"));
            }
        }
    }
    classify(url, request.call())
}

fn classify(
    url: &str,
    response: Result<ureq::Response, ureq::Error>,
) -> Result<ureq::Response, Failure> {
    response.map_err(|error| describe(url, &error))
}

fn describe(url: &str, error: &ureq::Error) -> Failure {
    let safe = endpoint(url);
    match error {
        ureq::Error::Status(status, response) => {
            let limited = url.starts_with("https://api.github.com/")
                && crate::github::rate_limited(*status, |h| response.header(h).map(str::to_owned));
            Failure {
                status: Some(*status),
                message: if limited {
                    trf(
                        "GitHub rate limit at {endpoint}. Try again later.",
                        &[("endpoint", &safe)],
                    )
                } else {
                    trf(
                        "Download server {endpoint} returned HTTP {status}.",
                        &[("endpoint", &safe), ("status", &status.to_string())],
                    )
                },
                retry: matches!(*status, 408 | 429 | 500 | 502 | 503 | 504) && !limited,
                after: retry_after(response.header("Retry-After")),
            }
        }
        ureq::Error::Transport(error) => {
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
            let mut timeout = false;
            while let Some(error) = source {
                timeout |= error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    )
                });
                source = error.source();
            }
            Failure {
                status: None,
                message: trf(
                    if timeout {
                        "Download from {endpoint} timed out."
                    } else {
                        "Could not connect to download server {endpoint}. Check your network and proxy settings."
                    },
                    &[("endpoint", &safe)],
                ),
                retry: true,
                after: None,
            }
        }
    }
}

/// Keep the original typed status for GitHub's conditional requests and cache fallback.
pub(crate) fn retry_request(
    url: &str,
    mut call: impl FnMut() -> Result<ureq::Response, Box<ureq::Error>>,
) -> Result<ureq::Response, Box<ureq::Error>> {
    for attempt in 0..ATTEMPTS {
        let result = call();
        if let Err(error) = &result {
            if pause(&describe(url, error.as_ref()), attempt, &std::thread::sleep) {
                continue;
            }
        }
        return result;
    }
    unreachable!()
}

fn pause(error: &Failure, attempt: usize, sleep: &dyn Fn(Duration)) -> bool {
    if !error.retry || attempt + 1 >= ATTEMPTS || error.after.is_some_and(|wait| wait > MAX_WAIT) {
        return false;
    }
    let mut random = [0u8; 2];
    let _ = getrandom::fill(&mut random);
    let jitter = Duration::from_millis(u16::from_ne_bytes(random) as u64 % 251);
    let backoff = Duration::from_millis(250 * (1 << attempt)) + jitter;
    sleep(error.after.unwrap_or_default().max(backoff));
    true
}

pub(crate) fn small(url: &str) -> Result<Option<Vec<u8>>, String> {
    for attempt in 0..ATTEMPTS {
        let result = request(url).and_then(|response| {
            let expected = response
                .header("Content-Length")
                .and_then(|n| n.parse::<usize>().ok());
            let mut body = Vec::new();
            let read = response
                .into_reader()
                .take((1 << 20) + 1)
                .read_to_end(&mut body);
            if body.len() > 1 << 20 {
                return Err(Failure {
                    status: None,
                    message: trf(
                        "Download from {endpoint} exceeded its expected size.",
                        &[("endpoint", &endpoint(url))],
                    ),
                    retry: false,
                    after: None,
                });
            }
            if read.is_ok() && expected.is_none_or(|n| n == body.len()) {
                return Ok(body);
            }
            Err(interrupted(url))
        });
        match result {
            Ok(body) => return Ok(Some(body)),
            Err(error) if error.status == Some(404) => return Ok(None),
            Err(error) if pause(&error, attempt, &std::thread::sleep) => {}
            Err(error) => return Err(error.message),
        }
    }
    unreachable!()
}

pub(crate) fn bytes(url: &str) -> Result<Vec<u8>, String> {
    bytes_with(url, || request(url), &std::thread::sleep)
}

fn bytes_with(
    url: &str,
    mut call: impl FnMut() -> Result<ureq::Response, Failure>,
    sleep: &dyn Fn(Duration),
) -> Result<Vec<u8>, String> {
    for attempt in 0..ATTEMPTS {
        let result = call().and_then(|response| {
            let expected = response
                .header("Content-Length")
                .and_then(|s| s.parse::<u64>().ok());
            let mut body = Vec::new();
            if response.into_reader().read_to_end(&mut body).is_ok()
                && expected.is_none_or(|n| n == body.len() as u64)
            {
                return Ok(body);
            }
            Err(interrupted(url))
        });
        match result {
            Ok(body) => return Ok(body),
            Err(error) if pause(&error, attempt, sleep) => {}
            Err(error) => return Err(error.message),
        }
    }
    unreachable!()
}

fn interrupted(url: &str) -> Failure {
    Failure { status: None, message: trf("Download from {endpoint} was interrupted. Retry the installation; verified files are kept.", &[("endpoint", &endpoint(url))]), retry: true, after: None }
}

#[cfg(test)]
mod tests;

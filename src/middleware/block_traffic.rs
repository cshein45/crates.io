use crate::app::AppState;
use crate::middleware::log_request::RequestLogExt;
use crate::middleware::real_ip::RealIp;
use crate::util::errors::{BoxedAppError, custom};
use axum::extract::{Extension, MatchedPath, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::{HeaderMap, StatusCode};
use regex::Regex;

pub async fn middleware(
    Extension(real_ip): Extension<RealIp>,
    matched_path: Option<MatchedPath>,
    state: AppState,
    req: Request,
    next: Next,
) -> Result<impl IntoResponse, Response> {
    block_by_ip(&real_ip, &state, req.headers()).map_err(IntoResponse::into_response)?;
    block_by_header(&state, &req).map_err(IntoResponse::into_response)?;
    block_routes(matched_path.as_ref(), &state).map_err(IntoResponse::into_response)?;

    Ok(next.run(req).await)
}

#[derive(Debug)]
pub enum BlockCriteria {
    Regex(Regex),
    String(String),
}

impl BlockCriteria {
    /// Create new criteria to use when deciding whether to block a request.
    ///
    /// - If the specified string starts and ends with `/` and has at least one character between
    ///   the slashes, interpret the value as a `Regex`.
    /// - Otherwise, interpret the value as an exact equality match.
    ///
    /// # Panics
    ///
    /// This will panic if the specified string is interpreted as a `Regex` but the regex is
    /// invalid.
    pub fn new(s: &str) -> Self {
        let is_regex = s.starts_with('/') && s.ends_with('/') && s.len() > 2;
        if is_regex {
            // Slicing is safe here because we checked the starting and ending characters and the
            // length before entering this branch
            Self::Regex(Regex::new(&s[1..s.len() - 1]).unwrap_or_else(|e| {
                panic!(
                    "BLOCKED_TRAFFIC values must be a valid regex after surrounding slashes are \
                     removed, got invalid regex {s}: {e}"
                )
            }))
        } else {
            Self::String(s.into())
        }
    }

    pub fn matches(&self, value: &str) -> bool {
        match self {
            Self::Regex(r) => r.is_match(value),
            Self::String(s) => s == value,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Regex(r) => r.as_str(),
            Self::String(s) => s,
        }
    }
}

impl From<&str> for BlockCriteria {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Middleware that blocks requests if a header matches the given criteria list
///
/// To use, set the `BLOCKED_TRAFFIC` environment variable to a comma-separated list of pairs
/// containing a header name, an equals sign, and the name of another environment variable that
/// contains the regex pattern values of that header that should be blocked.
///
/// For example, set `BLOCKED_TRAFFIC` to `User-Agent=BLOCKED_UAS` and `BLOCKED_UAS` to
/// `curl/[\d]+\.[\d]+\.[\d]+,cargo 1\.36\.0 \(c4fcfb725 2019-05-15\)` to block requests from any
/// version of curl and the exact version of Cargo specified (values are nonsensical examples).
///
/// Values of the headers must fully match the regex; that is, `^` and `$` are automatically added
/// around every regex specified.
pub fn block_by_header(state: &AppState, req: &Request) -> Result<(), impl IntoResponse> {
    let blocked_traffic = &state.config.blocked_traffic;

    for (header_name, blocked_values) in blocked_traffic {
        let has_blocked_value = req.headers().get_all(header_name).iter().any(|value| {
            value
                .to_str()
                .map(|ascii_val| blocked_values.iter().any(|v| v.matches(ascii_val)))
                .unwrap_or(false)
        });
        if has_blocked_value {
            let cause = format!("blocked due to contents of header {header_name}");
            req.request_log().add("cause", cause);

            return Err(rejection_response_from(state, req.headers()));
        }
    }

    Ok(())
}

pub fn block_by_ip(
    real_ip: &RealIp,
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), impl IntoResponse> {
    if state.config.blocked_ips.contains(real_ip) {
        return Err(rejection_response_from(state, headers));
    }

    Ok(())
}

fn rejection_response_from(state: &AppState, headers: &HeaderMap) -> impl IntoResponse {
    let domain_name = &state.config.domain_name;

    // Heroku should always set this header
    let request_id = headers
        .get("x-request-id")
        .map(|val| val.to_str().unwrap_or_default())
        .unwrap_or_default();

    let body = format!(
        "We are unable to process your request at this time. \
         This usually means that you are in violation of our API data access \
         policy (https://{domain_name}/data-access). \
         Please email help@crates.io and provide the request id {request_id}"
    );

    (StatusCode::FORBIDDEN, body)
}

/// Allow blocking individual routes by their pattern through the `BLOCKED_ROUTES`
/// environment variable.
pub fn block_routes(
    matched_path: Option<&MatchedPath>,
    state: &AppState,
) -> Result<(), BoxedAppError> {
    if let Some(matched_path) = matched_path
        && state.config.blocked_routes.contains(matched_path.as_str())
    {
        let body = "This route is temporarily blocked. See https://status.crates.io.";
        return Err(custom(StatusCode::SERVICE_UNAVAILABLE, body));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "BLOCKED_TRAFFIC values must be a valid regex")]
    fn new_block_criteria_panics_on_invalid_regex() {
        BlockCriteria::new("/)/");
    }

    #[test]
    fn new_block_criteria_string_vs_regex() {
        let one_slash = BlockCriteria::new("/");
        assert!(
            matches!(one_slash, BlockCriteria::String(_)),
            "Expected BlockCriteria::String, got {one_slash:?}",
        );

        let two_slashes_no_content = BlockCriteria::new("//");
        assert!(
            matches!(two_slashes_no_content, BlockCriteria::String(_)),
            "Expected BlockCriteria::String, got {two_slashes_no_content:?}",
        );

        let starting_slash_only = BlockCriteria::new("/hello i am not regex");
        assert!(
            matches!(starting_slash_only, BlockCriteria::String(_)),
            "Expected BlockCriteria::String, got {starting_slash_only:?}",
        );

        let ending_slash_only = BlockCriteria::new("hello me neither//");
        assert!(
            matches!(ending_slash_only, BlockCriteria::String(_)),
            "Expected BlockCriteria::String, got {ending_slash_only:?}",
        );

        let string_doesnt_need_to_be_valid_regex = BlockCriteria::new("+");
        assert!(
            matches!(
                string_doesnt_need_to_be_valid_regex,
                BlockCriteria::String(_)
            ),
            "Expected BlockCriteria::String, got {string_doesnt_need_to_be_valid_regex:?}",
        );

        let now_thats_what_i_call_regex = BlockCriteria::new("/yes this is regex/");
        assert!(
            matches!(now_thats_what_i_call_regex, BlockCriteria::Regex(_)),
            "Expected BlockCriteria::Regex, got {now_thats_what_i_call_regex:?}",
        );
    }
}

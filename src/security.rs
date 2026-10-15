use axum::{
    extract::Request,
    http::{header, uri::Authority, HeaderValue, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Explicit proxy hostnames, not URLs, wildcards, or forwarded-header values.
pub fn parse_host(value: &str) -> Result<String, String> {
    let authority: Authority = value
        .parse()
        .map_err(|_| "Expected a hostname without a scheme or port")?;
    if authority.port().is_some() || value.contains(['@', '*']) || authority.host().is_empty() {
        return Err("Expected a hostname without a scheme or port".into());
    }
    Ok(authority.host().to_ascii_lowercase())
}

pub async fn protect(request: Request, next: Next, allowed_hosts: &[String]) -> Response {
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let authority = hosts
        .next()
        .filter(|_| hosts.next().is_none())
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.contains('@'))
        .and_then(|value| value.parse::<Authority>().ok());
    let trusted = authority.as_ref().is_some_and(|authority| {
        let host = authority.host();
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
            || allowed_hosts
                .iter()
                .any(|allowed| host.eq_ignore_ascii_case(allowed))
    });
    let cross_site = request
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|value| value == "cross-site");
    let foreign_origin = request.headers().get(header::ORIGIN).is_some_and(|value| {
        let origin = value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<Uri>().ok());
        !(request.headers().get_all(header::ORIGIN).iter().count() == 1
            && origin.as_ref().is_some_and(|origin| {
                matches!(origin.scheme_str(), Some("http" | "https"))
                    && origin.authority() == authority.as_ref()
            }))
    });
    let mut response = if !trusted
        || foreign_origin
        || (request.uri().path().starts_with("/api/") && cross_site)
    {
        (StatusCode::FORBIDDEN, [(header::CACHE_CONTROL, "no-store")],
            axum::Json(serde_json::json!({"error": "Untrusted host or cross-origin request. Configure --allow-host for a private reverse proxy."}))).into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

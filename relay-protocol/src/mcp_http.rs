//! Canonical HTTP forwarding policy for the public Relay and Desktop WSS
//! proxy. Both executables must accept exactly the same paths and headers.
pub const WSS_PROTOCOL_V1: u8 = 1;
pub const WSS_PROTOCOL_V2: u8 = 2;
pub const LEGACY_MAX_REQUEST_BYTES: usize = 5 * 1024 * 1024;
pub const LEGACY_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

pub fn allowed_path(path: &str) -> bool {
    matches!(path,
        "/mcp" |
        "/chatx-oauth/authorize" | "/chatx-oauth/approve" |
        "/chatx-oauth/token" | "/chatx-oauth/register" |
        "/.well-known/oauth-authorization-server" |
        "/.well-known/oauth-protected-resource" |
        "/.well-known/oauth-protected-resource/mcp")
}

pub fn allowed_request_header(name: &str) -> bool {
    matches!(name,
        "content-type" | "accept" | "authorization" |
        "mcp-session-id" | "mcp-protocol-version" | "last-event-id" |
        "x-forwarded-for" | "user-agent" | "origin" | "accept-language")
}

pub fn allowed_response_header(name: &str) -> bool {
    matches!(name,
        "content-type" | "cache-control" | "www-authenticate" |
        "location" | "mcp-session-id" | "retry-after" |
        "x-content-type-options" | "content-security-policy" |
        "x-frame-options" | "referrer-policy" | "allow" |
        "access-control-allow-origin" | "access-control-allow-methods" |
        "access-control-allow-headers" | "access-control-expose-headers" |
        "vary")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_mcp_and_oauth_routes_are_shared() {
        for path in [
            "/mcp", "/chatx-oauth/authorize", "/chatx-oauth/approve",
            "/chatx-oauth/token", "/chatx-oauth/register",
            "/.well-known/oauth-authorization-server",
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
        ] {
            assert!(allowed_path(path), "{path}");
        }
        for path in ["/", "/admin", "/healthz", "/mcp/extra",
            "//mcp", "/chatx-oauth/approve/extra"] {
            assert!(!allowed_path(path), "{path}");
        }
    }

    #[test]
    fn forwarding_headers_are_explicitly_allowlisted() {
        for name in ["authorization", "content-type", "origin", "x-forwarded-for"] {
            assert!(allowed_request_header(name));
        }
        for name in ["www-authenticate", "location", "retry-after"] {
            assert!(allowed_response_header(name));
        }
        for name in ["cookie", "set-cookie", "host", "connection", "x-real-ip"] {
            assert!(!allowed_request_header(name));
            assert!(!allowed_response_header(name));
        }
    }
}

//! Shared MCP WSS control-message discriminator values. Payloads stay JSON
//! for backwards-compatible v1/v2 framing; binary bodies use mcp_stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Hello,
    HelloAck,
    Ping,
    Pong,
    Request,
    RequestEnd,
    RequestCancel,
    Head,
    Chunk,
    End,
    Error,
}
impl ControlKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::HelloAck => "hello_ack",
            Self::Ping => "ping",
            Self::Pong => "pong",
            Self::Request => "request",
            Self::RequestEnd => "request_end",
            Self::RequestCancel => "request_cancel",
            Self::Head => "head",
            Self::Chunk => "chunk",
            Self::End => "end",
            Self::Error => "error",
        }
    }

    pub fn parse(kind: &str) -> Option<Self> {
        Some(match kind {
            "hello" => Self::Hello,
            "hello_ack" => Self::HelloAck,
            "ping" => Self::Ping,
            "pong" => Self::Pong,
            "request" => Self::Request,
            "request_end" => Self::RequestEnd,
            "request_cancel" => Self::RequestCancel,
            "head" => Self::Head,
            "chunk" => Self::Chunk,
            "end" => Self::End,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_v2_control_kinds_roundtrip_without_schema_changes() {
        for kind in [
            ControlKind::Hello, ControlKind::HelloAck,
            ControlKind::Ping, ControlKind::Pong,
            ControlKind::Request, ControlKind::RequestEnd,
            ControlKind::RequestCancel, ControlKind::Head,
            ControlKind::Chunk, ControlKind::End, ControlKind::Error,
        ] {
            assert_eq!(ControlKind::parse(kind.as_str()), Some(kind));
        }
        for unsupported in ["", "binary", "request_cancelled", "admin"] {
            assert_eq!(ControlKind::parse(unsupported), None);
        }
    }
}

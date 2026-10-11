//! Binary MCP WSS v2 frames (1 byte kind + 16 raw request-id bytes + body).
//! No Base64 overhead. The existing v1 JSON framing stays wire-compatible.
pub const MAX_FRAME_BODY: usize = 64 * 1024;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_INFLIGHT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    RequestBody = 1,
    ResponseBody = 2,
}
pub fn encode(kind: Kind, request_id: &str, body: &[u8]) -> Option<Vec<u8>> {
    if request_id.len() != 32 || body.len() > MAX_FRAME_BODY { return None; }
    let mut frame = Vec::with_capacity(17 + body.len());
    frame.push(kind as u8);
    for pair in request_id.as_bytes().chunks_exact(2) {
        let raw = std::str::from_utf8(pair).ok()?;
        frame.push(u8::from_str_radix(raw, 16).ok()?);
    }
    frame.extend_from_slice(body);
    Some(frame)
}
pub fn decode(frame: &[u8]) -> Option<(Kind, String, &[u8])> {
    if frame.len() < 17 || frame.len() > 17 + MAX_FRAME_BODY { return None; }
    let kind = match frame[0] {
        1 => Kind::RequestBody,
        2 => Kind::ResponseBody,
        _ => return None,
    };
    let mut id = String::with_capacity(32);
    for byte in &frame[1..17] {
        use std::fmt::Write;
        write!(&mut id, "{byte:02x}").ok()?;
    }
    Some((kind, id, &frame[17..]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_frame_round_trip_and_limits() {
        let id = "a1".repeat(16);
        let data = vec![207; MAX_FRAME_BODY];
        let packet = encode(Kind::ResponseBody, &id, &data).unwrap();
        assert_eq!(packet.len(), data.len() + 17);
        let (kind, recovered, body) = decode(&packet).unwrap();
        assert_eq!((kind, recovered), (Kind::ResponseBody, id.clone()));
        assert_eq!(body, data);
        assert!(encode(Kind::RequestBody, &id, &[0; MAX_FRAME_BODY + 1]).is_none());
        assert!(decode(&[0; 17]).is_none());
        assert!(decode(&[1; 16]).is_none());
    }
}

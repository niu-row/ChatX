// Per-Bridge admission accounting for buffered MCP JSON requests.
// Tracks wire payload bytes, not actual V8 heap usage or upstream tool output.
export function createMcpRequestBudget({
  maxRequests = 12, maxTools = 8, maxPayloadBytes = 80 * 1024 * 1024,
} = {}) {
  let requests = 0;
  let tools = 0;
  let payloadBytes = 0;
  function open() {
    if (requests >= maxRequests) return null;
    requests++;
    let heldBytes = 0;
    let tool = false;
    let closed = false;
    return {
      reserve(bytes) {
        if (closed || !Number.isSafeInteger(bytes) || bytes < 0 ||
            payloadBytes + bytes > maxPayloadBytes) return false;
        payloadBytes += bytes;
        heldBytes += bytes;
        return true;
      },
      admitTool() {
        if (closed || tool || tools >= maxTools) return false;
        tools++;
        tool = true;
        return true;
      },
      close() {
        if (closed) return;
        closed = true;
        requests--;
        if (tool) tools--;
        payloadBytes -= heldBytes;
      },
    };
  }
  return { open, snapshot: () => ({ requests, tools, payloadBytes }) };
}

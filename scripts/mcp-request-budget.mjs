// Separate provisional HTTP body readers from lightweight MCP controls.
// Admission is based on a small-body hint, but the method is verified after
// parsing: large/slow tool calls must never keep a reserved control slot.
// This accounts for wire bytes / serialized responses, not SDK/V8 heap.
export function createMcpRequestBudget({
  maxRequests = 12,
  maxControl = 4,
  maxTools = 8,
  maxPayloadBytes = 80 * 1024 * 1024,
  maxControlBodyBytes = 4096,
  maxSingleResponseBytes = 128 * 1024 * 1024,
  maxBufferedResponseBytes = 128 * 1024 * 1024,
} = {}) {
  if (!Number.isSafeInteger(maxRequests) || !Number.isSafeInteger(maxControl) ||
      maxRequests < 1 || maxControl < 0 || maxControl >= maxRequests) {
    throw new Error('Invalid MCP admission budget');
  }
  const normalLimit = maxRequests - maxControl;
  const controlMethods = new Set(['initialize', 'ping',
    'notifications/initialized', 'notifications/cancelled']);
  let normal = 0;
  let control = 0;
  let tools = 0;
  let payloadBytes = 0;
  let responseBytes = 0;

  function open({ controlCandidate = false } = {}) {
    let lane;
    if (controlCandidate && control < maxControl) lane = 'control';
    else if (normal < normalLimit) lane = 'normal';
    else return null;
    if (lane === 'control') control++;
    else normal++;
    let heldBytes = 0;
    let heldResponse = 0;
    let tool = false;
    let closed = false;
    return {
      get controlCandidate() { return lane === 'control'; },
      get bodyLimit() { return lane === 'control' ? maxControlBodyBytes : 64 * 1024 * 1024; },
      reserve(bytes) {
        if (closed || !Number.isSafeInteger(bytes) || bytes < 0 ||
            (lane === 'control' && heldBytes + bytes > maxControlBodyBytes) ||
            payloadBytes + bytes > maxPayloadBytes) return false;
        payloadBytes += bytes;
        heldBytes += bytes;
        return true;
      },
      admit(method) {
        if (closed) return false;
        const quick = controlMethods.has(method);
        // A short tools/call is still a TOOL, not a control message. It must
        // switch lanes before running (or fail 429 if all normal slots are full).
        if (lane === 'control' && !quick) {
          if (normal >= normalLimit) return false;
          control--;
          normal++;
          lane = 'normal';
        } else if (lane === 'normal' && quick && control < maxControl) {
          normal--;
          control++;
          lane = 'control';
        }
        if (method === 'tools/call' && !tool) {
          if (tools >= maxTools) return false;
          tools++;
          tool = true;
        }
        return true;
      },
      admitTool() { return this.admit('tools/call'); },
      reserveResponse(bytes) {
        if (closed || heldResponse || !Number.isSafeInteger(bytes) || bytes < 0 ||
            bytes > maxSingleResponseBytes ||
            responseBytes + bytes > maxBufferedResponseBytes) return false;
        heldResponse = bytes;
        responseBytes += bytes;
        return true;
      },
      releaseResponse() {
        responseBytes -= heldResponse;
        heldResponse = 0;
      },
      close() {
        if (closed) return;
        closed = true;
        if (lane === 'control') control--;
        else normal--;
        if (tool) tools--;
        payloadBytes -= heldBytes;
        // Buffered response bytes are retained until HTTP finish/close and
        // released separately by releaseResponse().
      },
    };
  }
  return {
    open,
    snapshot: () => ({
      requests: normal + control, normal, control, tools,
      payloadBytes, responseBytes,
    }),
  };
}

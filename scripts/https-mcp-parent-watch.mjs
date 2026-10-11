// macOS process-group watchdog for the HTTPS MCP bridge and SSH forward.
// The GUI's PID is passed explicitly so an orphaned startup cannot succeed.
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// Also testable without spawning processes (Windows cannot send POSIX
// process-group signals). In particular, child 'exit' must NOT cancel SIGKILL.
export function createShutdownCoordinator({ sendSignal, stopWatching, schedule, finish }) {
  let stopping = false;
  return function shutdown(exitCode = 0) {
    if (stopping) return;
    stopping = true;
    stopWatching();
    sendSignal('SIGTERM');
    // The timeout must stay referenced: after the leader exits there may be
    // no other handles, but descendants may still ignore SIGTERM.
    schedule(() => {
      sendSignal('SIGKILL');
      finish(exitCode);
    }, 1500);
  };
}

function runWatchdog() {
  const [expectedParentText, executable, ...args] = process.argv.slice(2);
  const expectedParent = Number(expectedParentText);
  if (!Number.isSafeInteger(expectedParent) || expectedParent < 2 || !executable ||
      process.ppid !== expectedParent) {
    console.error('ChatX parent-watch: parent PID mismatch');
    process.exit(2);
  }

  const child = spawn(executable, args, {
    detached: true, // The launched process leads a dedicated process group.
    stdio: ['ignore', 'ignore', 'inherit'], env: process.env
  });
  let watch;
  function sendGroup(signal) {
    if (!child.pid) return;
    try { process.kill(-child.pid, signal); }
    catch (error) { if (error.code !== 'ESRCH') console.error(error.message); }
  }
  const shutdown = createShutdownCoordinator({
    sendSignal: sendGroup,
    stopWatching: () => clearInterval(watch),
    schedule: (callback, delay) => setTimeout(callback, delay),
    finish: (code) => process.exit(code)
  });
  watch = setInterval(() => {
    if (process.ppid !== expectedParent) shutdown(0);
  }, 700);
  watch.unref();

  process.on('SIGTERM', () => shutdown(0));
  process.on('SIGINT', () => shutdown(0));
  process.on('SIGHUP', () => shutdown(0));
  child.on('error', (error) => {
    console.error('ChatX parent-watch child:', error.message);
    shutdown(1);
  });
  child.on('exit', (code, signal) => {
    // A leader can exit while SSH/Node descendants remain in its process
    // group. Always complete the escalation before exiting the watchdog.
    shutdown(code ?? (signal ? 1 : 0));
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runWatchdog();
}

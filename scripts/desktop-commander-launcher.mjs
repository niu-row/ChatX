import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [, , homeArg, entryArg, ...serverArgs] = process.argv;
if (!homeArg || !entryArg) {
  console.error('ChatX Desktop Commander launcher requires <home> <entry> [args...]');
  process.exit(2);
}

const home = path.resolve(homeArg);
const entry = path.resolve(entryArg);
fs.mkdirSync(home, { recursive: true });

// CHATX_TUNNEL_RUNTIME_KEY is only for tunnel-client authentication. The MCP
// process must not retain it because Desktop Commander can launch arbitrary
// child processes that would otherwise inherit the secret.
delete process.env.CHATX_TUNNEL_RUNTIME_KEY;

// Desktop Commander derives its config path from os.homedir(). Keep the bundled
// ChatX instance isolated from any separately installed Desktop Commander.
process.env.HOME = home;
process.env.USERPROFILE = home;
// Hard upstream kill-switch: do not send Desktop Commander telemetry from ChatX.
process.env.DESKTOP_COMMANDER_DISABLE_TELEMETRY = '1';

process.argv = [process.execPath, entry, ...serverArgs];
await import(pathToFileURL(entry).href);

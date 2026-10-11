// Local-only administrative entry point for the same OAuth grant store used
// by the HTTPS MCP bridge. No server, network access or bearer secrets.
import { authorizationStore } from './https-mcp-authorizations.mjs';

const [action, grantId] = process.argv.slice(2);
if (!['list', 'revoke', 'revoke-all'].includes(action) ||
    (action === 'revoke' && !/^(legacy|[0-9a-f]{64})$/.test(grantId || '')) ||
    (action !== 'revoke' && grantId !== undefined)) {
  throw new Error('Invalid local OAuth administration command');
}
const store = authorizationStore({
  directory: process.env.CHATX_MCP_AUTH_STORE,
  settingsPath: process.env.CHATX_MCP_SETTINGS_PATH,
  createLegacyIfMissing: false,
});
const response = action === 'list' ? store.listGrants()
  : action === 'revoke' ? store.revokeGrant(grantId)
    : store.revokeAll();
process.stdout.write(JSON.stringify(response) + '\n');

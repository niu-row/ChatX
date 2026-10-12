// Persistent OAuth authorization grants; no bearer tokens or passwords are stored.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

const HOUR = 3600;
const LEGACY_GRACE_SECONDS = 7 * 24 * HOUR;
const validGrantId = (value) => /^[0-9a-f]{64}$/.test(value || '');
const clientHash = (clientId) =>
  crypto.createHash('sha256').update(clientId).digest('hex');
const readJson = (file) => JSON.parse(fs.readFileSync(file, 'utf8'));
const nowSec = () => Math.floor(Date.now() / 1000);

export function authorizationStore({ directory, settingsPath, createLegacyIfMissing = true }) {
  if (!path.isAbsolute(directory || '') || !path.isAbsolute(settingsPath || '')) {
    throw new Error('OAuth authorization store requires absolute local paths');
  }
  const grantsDir = path.join(directory, 'grants');
  const revokedDir = path.join(directory, 'revoked');
  const refreshDir = path.join(directory, 'refresh');
  for (const folder of [directory, grantsDir, revokedDir, refreshDir]) {
    fs.mkdirSync(folder, { recursive: true, mode: 0o700 });
  }
  const legacyPath = path.join(directory, 'legacy.json');
  const revokedAllPath = path.join(directory, 'revoked-all.json');
  if (createLegacyIfMissing && !fs.existsSync(legacyPath)) {
    try {
      fs.writeFileSync(legacyPath,
        JSON.stringify({ expiresAt: nowSec() + LEGACY_GRACE_SECONDS }),
        { flag: 'wx', mode: 0o600 });
    } catch (error) { if (error.code !== 'EEXIST') throw error; }
  }
  // Existing OAuth tokens have no grant ID. Allow them only during a
  // one-time migration window, then require a fresh consent.
  const legacyRecord = fs.existsSync(legacyPath) ? readJson(legacyPath) : null;
  if (legacyRecord && !Number.isSafeInteger(legacyRecord.expiresAt)) {
    throw new Error('Invalid legacy OAuth migration cutoff');
  }
  function lifetimeHours() {
    const settings = readJson(settingsPath);
    const hours = settings.httpsMcp?.authorizationTtlHours;
    if (hours === undefined) return 7 * 24;
    if (hours === null) return null;
    if (!Number.isSafeInteger(hours) || hours < 1 || hours > 24 * 365 * 10) {
      throw new Error('OAuth authorization lifetime must be 1..87600 hours or null');
    }
    return hours;
  }
  function revocationEpoch() {
    if (!fs.existsSync(revokedAllPath)) return 0;
    const value = readJson(revokedAllPath).revokedAtMs;
    if (!Number.isSafeInteger(value) || value <= 0) {
      throw new Error('OAuth global revocation record is corrupt');
    }
    return value;
  }
  function revocationGeneration() {
    if (!fs.existsSync(revokedAllPath)) return 'initial';
    const marker = readJson(revokedAllPath);
    if (typeof marker.generation === 'string' && /^[0-9a-f]{64}$/.test(marker.generation)) {
      return marker.generation;
    }
    if (Number.isSafeInteger(marker.revokedAtMs) && marker.revokedAtMs > 0) {
      return `old:${marker.revokedAtMs}`;
    }
    throw new Error('OAuth revocation generation is corrupt');
  }
  function createGrant(clientId, ttlHours, approvedAtMs, approvedGeneration) {
    if (ttlHours !== null && (!Number.isSafeInteger(ttlHours)
        || ttlHours < 1 || ttlHours > 24 * 365 * 10)) {
      throw new Error('Invalid approved OAuth lifetime');
    }
    if (!Number.isSafeInteger(approvedAtMs) ||
        approvedGeneration !== revocationGeneration()) {
      throw new Error('OAuth authorization code was revoked before exchange');
    }
    const id = crypto.randomBytes(32).toString('hex');
    const issuedAt = nowSec();
    const record = { id, clientHash: clientHash(clientId), issuedAt, approvedAtMs,
      revocationGeneration: approvedGeneration,
      expiresAt: ttlHours === null ? null : issuedAt + ttlHours * HOUR };
    fs.writeFileSync(path.join(grantsDir, id + '.json'), JSON.stringify(record),
      { flag: 'wx', mode: 0o600 });
    return record;
  }
  // Both bearer-token verification and the authorization UI must use the
  // same revocation predicate, including pre-generation migration records.
  function grantRevoked(record, id) {
    if (fs.existsSync(path.join(revokedDir, id))) return true;
    return record.revocationGeneration === undefined
      ? (record.approvedAtMs ?? record.issuedAt * 1000) <= revocationEpoch()
      : record.revocationGeneration !== revocationGeneration();
  }
  function activeGrant(id, clientId = null) {
    if (id === undefined || id === null) {
      if (!legacyRecord || revocationEpoch() > 0 ||
          fs.existsSync(path.join(revokedDir, 'legacy'))) return null;
      return legacyRecord.expiresAt > nowSec()
        ? { id: 'legacy', expiresAt: legacyRecord.expiresAt } : null;
    }
    if (!validGrantId(id) || fs.existsSync(path.join(revokedDir, id))) return null;
    let record;
    try {
      record = readJson(path.join(grantsDir, id + '.json'));
    } catch (error) {
      if (error.code === 'ENOENT') return null;
      throw error; // fail closed on corruption or unreadable storage
    }
    if (record.id !== id || typeof record.clientHash !== 'string' ||
        (record.expiresAt !== null && (!Number.isSafeInteger(record.expiresAt)
          || record.expiresAt <= nowSec())) ||
        grantRevoked(record, id)) return null;
    if (clientId !== null && record.clientHash !== clientHash(clientId)) return null;
    return record;
  }
  // Lost HTTP responses are normal: for a short period a retry of the same
  // signed refresh token returns the *same* successor, without rotating twice.
  // Beyond the retry window, a stale token is treated as replay and revokes
  // the grant. Persist only hashed JTIs, not bearer credentials.
  const REFRESH_RETRY_MS = 30_000;
  const hashJti = (jti) => crypto.createHash('sha256').update(jti).digest('hex');
  function advanceRefresh(grantId, priorJti, nextJti, issuedAt) {
    const id = grantId || 'legacy';
    if (id !== 'legacy' && !validGrantId(id)) throw new Error('invalid_grant');
    if (!activeGrant(id === 'legacy' ? undefined : id)) throw new Error('invalid_grant');
    if (typeof nextJti !== 'string' || !/^[A-Za-z0-9_-]{43,64}$/.test(nextJti) ||
        !Number.isSafeInteger(issuedAt)) throw new Error('invalid_grant');
    const file = path.join(refreshDir, id + '.json');
    const current = fs.existsSync(file) ? readJson(file) : null;
    const priorTag = priorJti === null || priorJti === undefined ? null : hashJti(String(priorJti));
    const nextTag = hashJti(nextJti);
    const now = Date.now();
    if (current && current.previousTag === priorTag && current.tag === nextTag &&
        Number.isSafeInteger(current.retryUntilMs) && current.retryUntilMs >= now &&
        Number.isSafeInteger(current.issuedAt)) {
      return { jti: nextJti, issuedAt: current.issuedAt, retried: true };
    }
    if ((current?.tag ?? null) !== priorTag || (!current && priorTag !== null)) {
      fs.writeFileSync(path.join(revokedDir, id), 'refresh_replay', { mode: 0o600 });
      throw new Error('invalid_grant');
    }
    const temp = file + '.' + crypto.randomBytes(12).toString('hex') + '.tmp';
    try {
      fs.writeFileSync(temp, JSON.stringify({
        tag: nextTag, previousTag: priorTag, issuedAt,
        retryUntilMs: now + REFRESH_RETRY_MS
      }), { flag: 'wx', mode: 0o600 });
      fs.renameSync(temp, file);
    } finally {
      try { fs.unlinkSync(temp); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
    return { jti: nextJti, issuedAt, retried: false };
  }
  function expiresAt(grant) {
    return grant?.expiresAt ?? null;
  }

  // The authorization store is the ONLY owner of persisted grant formats.
  // The desktop administrative command invokes these methods through a local
  // bundled Node CLI; Rust no longer parses or writes grant records.
  function listGrants() {
    const generation = fs.existsSync(revokedAllPath) ? revocationGeneration() : null;
    const grants = [];
    for (const entry of fs.readdirSync(grantsDir, { withFileTypes: true })) {
      if (!entry.isFile() || !entry.name.endsWith('.json')) continue;
      const record = readJson(path.join(grantsDir, entry.name));
      const id = record.id;
      if (!validGrantId(id) || entry.name !== id + '.json') {
        throw new Error('Invalid OAuth grant ID in persisted store');
      }
      grants.push({
        id,
        clientTag: typeof record.clientHash === 'string' ? record.clientHash : '',
        issuedAt: record.issuedAt,
        expiresAt: record.expiresAt,
        revoked: grantRevoked(record, id),
      });
    }
    if (fs.existsSync(legacyPath)) {
      grants.push({
        id: 'legacy',
        clientTag: '旧版授权（需重新授权以选择有效期）',
        issuedAt: 0,
        expiresAt: (legacyRecord ?? readJson(legacyPath)).expiresAt,
        revoked: generation !== null || fs.existsSync(path.join(revokedDir, 'legacy')),
      });
    }
    grants.sort((a, b) => (b.issuedAt ?? 0) - (a.issuedAt ?? 0));
    return { grants };
  }
  function revokeGrant(id) {
    if (id !== 'legacy' && !validGrantId(id)) throw new Error('Invalid OAuth grant ID');
    const file = id === 'legacy' ? legacyPath : path.join(grantsDir, id + '.json');
    if (!fs.existsSync(file)) throw new Error('OAuth grant does not exist');
    fs.writeFileSync(path.join(revokedDir, id), 'revoked', { mode: 0o600 });
    // Confirm the mutation independently of listGrants(): unrelated damaged
    // records must not turn a successful revocation into a reported failure.
    return { revokedId: id };
  }
  function revokeAll() {
    // A new generation invalidates existing grants and pending OAuth codes
    // while allowing new consent after this point. Write atomically.
    const generation = crypto.randomBytes(32).toString('hex');
    const next = path.join(directory, 'revoked-all.' + generation + '.tmp');
    try {
      fs.writeFileSync(next, JSON.stringify({
        revokedAtMs: Date.now(), generation,
      }), { flag: 'wx', mode: 0o600 });
      fs.renameSync(next, revokedAllPath);
    } finally {
      try { fs.unlinkSync(next); } catch (e) { if (e.code !== 'ENOENT') throw e; }
    }
    return { revokedAll: true };
  }
  return {
    lifetimeHours, createGrant, activeGrant, expiresAt, revocationGeneration,
    advanceRefresh, listGrants, revokeGrant, revokeAll,
  };
}

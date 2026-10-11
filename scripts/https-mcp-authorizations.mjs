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

export function authorizationStore({ directory, settingsPath }) {
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
  if (!fs.existsSync(legacyPath)) {
    try {
      fs.writeFileSync(legacyPath,
        JSON.stringify({ expiresAt: nowSec() + LEGACY_GRACE_SECONDS }),
        { flag: 'wx', mode: 0o600 });
    } catch (error) { if (error.code !== 'EEXIST') throw error; }
  }
  // Existing OAuth tokens have no grant ID. Allow them only during a
  // one-time migration window, then require a fresh consent.
  const legacyRecord = readJson(legacyPath);
  if (!Number.isSafeInteger(legacyRecord.expiresAt)) {
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
  function activeGrant(id, clientId = null) {
    if (id === undefined || id === null) {
      if (revocationEpoch() > 0 || fs.existsSync(path.join(revokedDir, 'legacy'))) return null;
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
        (record.revocationGeneration === undefined
          ? (record.approvedAtMs ?? record.issuedAt * 1000) <= revocationEpoch()
          : record.revocationGeneration !== revocationGeneration())) return null;
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
  return { lifetimeHours, createGrant, activeGrant, expiresAt, revocationGeneration, advanceRefresh };
}

import fs from 'node:fs/promises';
import path from 'node:path';
import { getPathPolicySettings } from '../settings.js';

function comparable(value: string): string {
  const normalized = path.resolve(value);
  return process.platform === 'win32' ? normalized.toLowerCase() : normalized;
}

function isWithin(base: string, target: string): boolean {
  const a = comparable(base);
  const b = comparable(target);
  if (a === b) return true;
  const relative = path.relative(a, b);
  return relative !== '' && !relative.startsWith('..' + path.sep) && relative !== '..' && !path.isAbsolute(relative);
}

async function realpathOrResolved(value: string): Promise<string> {
  try {
    return await fs.realpath(value);
  } catch {
    return path.resolve(value);
  }
}

async function nearestExistingAncestor(value: string): Promise<string> {
  let current = path.resolve(value);
  for (;;) {
    try {
      await fs.lstat(current);
      return current;
    } catch {
      const parent = path.dirname(current);
      if (parent === current) return current;
      current = parent;
    }
  }
}

let cachedRevision = -1;
let cachedRealRoots: Promise<string[]> | null = null;

async function realRootsFor(revision: number, roots: string[]): Promise<string[]> {
  if (revision !== cachedRevision || !cachedRealRoots) {
    cachedRevision = revision;
    cachedRealRoots = Promise.all(roots.map((root) => realpathOrResolved(root)));
  }
  return await cachedRealRoots;
}

async function allowedByRealPath(target: string, realRoots: string[]): Promise<boolean> {
  const ancestor = await nearestExistingAncestor(target);
  const realAncestor = await realpathOrResolved(ancestor);
  return realRoots.some((realRoot) => isWithin(realRoot, realAncestor));
}

export async function assertPathAllowed(inputPath: string): Promise<string> {
  if (!inputPath || inputPath.includes('\0')) {
    throw new Error('Invalid path.');
  }

  const resolved = path.resolve(inputPath);
  const policy = getPathPolicySettings();
  if (policy.fullAccess) return resolved;

  const lexicallyAllowed = policy.roots.some((root) => isWithin(root, resolved));
  const realRoots = lexicallyAllowed ? await realRootsFor(policy.revision, policy.roots) : [];
  if (!lexicallyAllowed || !(await allowedByRealPath(resolved, realRoots))) {
    throw new Error(
      `Path is outside configured roots: ${resolved}. Allowed roots: ${policy.roots.join(', ')}. ` +
        'Enable full filesystem access only if you intentionally want unrestricted access.',
    );
  }

  return resolved;
}

export async function assertExistingPath(inputPath: string): Promise<string> {
  const resolved = await assertPathAllowed(inputPath);
  await fs.access(resolved);
  return resolved;
}

export function describePathPolicy(): { fullAccess: boolean; roots: string[] } {
  const policy = getPathPolicySettings();
  return { fullAccess: policy.fullAccess, roots: policy.roots };
}

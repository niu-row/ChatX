import fs from 'node:fs/promises';
import path from 'node:path';
import { getRuntimeSettings } from '../settings.js';

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

function configuredRoots(): string[] {
  return getRuntimeSettings().filesystem.roots;
}

async function allowedByRealPath(target: string, roots: string[]): Promise<boolean> {
  const ancestor = await nearestExistingAncestor(target);
  const realAncestor = await realpathOrResolved(ancestor);

  for (const configuredRoot of roots) {
    const realRoot = await realpathOrResolved(configuredRoot);
    if (isWithin(realRoot, realAncestor)) return true;
  }
  return false;
}

export async function assertPathAllowed(inputPath: string): Promise<string> {
  if (!inputPath || inputPath.includes('\0')) {
    throw new Error('Invalid path.');
  }

  const resolved = path.resolve(inputPath);
  const runtime = getRuntimeSettings();
  if (runtime.permissions.fullAccess) return resolved;

  const roots = configuredRoots();
  const lexicallyAllowed = roots.some((root) => isWithin(root, resolved));
  if (!lexicallyAllowed || !(await allowedByRealPath(resolved, roots))) {
    throw new Error(
      `Path is outside configured roots: ${resolved}. Allowed roots: ${roots.join(', ')}. ` +
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
  const runtime = getRuntimeSettings();
  return { fullAccess: runtime.permissions.fullAccess, roots: [...runtime.filesystem.roots] };
}

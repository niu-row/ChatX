import fs from 'node:fs/promises';
import path from 'node:path';
import { config } from '../config.js';

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

async function allowedByRealPath(target: string): Promise<boolean> {
  const ancestor = await nearestExistingAncestor(target);
  const realAncestor = await realpathOrResolved(ancestor);

  for (const configuredRoot of config.roots) {
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
  if (config.fullAccess) return resolved;

  const lexicallyAllowed = config.roots.some((root) => isWithin(root, resolved));
  if (!lexicallyAllowed || !(await allowedByRealPath(resolved))) {
    throw new Error(
      `Path is outside configured roots: ${resolved}. Allowed roots: ${config.roots.join(', ')}. ` +
        'Set CHATGPTX_FULL_ACCESS=true only if you intentionally want unrestricted filesystem access.',
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
  return { fullAccess: config.fullAccess, roots: [...config.roots] };
}

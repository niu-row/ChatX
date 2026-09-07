import fs from 'node:fs/promises';
import path from 'node:path';

function contains(parent: string, child: string): boolean {
  const normalize = (value: string) => process.platform === 'win32' ? value.toLowerCase() : value;
  const relative = path.relative(normalize(parent), normalize(child));
  return relative === '' || (!path.isAbsolute(relative) && relative !== '..' && !relative.startsWith('..' + path.sep));
}

async function canonicalFuture(value: string): Promise<string> {
  try { return await fs.realpath(value); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    const parent = path.dirname(value);
    if (parent === value) throw error;
    return path.join(await canonicalFuture(parent), path.basename(value));
  }
}

async function exists(value: string): Promise<boolean> {
  try { await fs.lstat(value); return true; }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    return false;
  }
}

export async function safeMove(source: string, destination: string, overwrite: boolean, createParents: boolean) {
  const [realSource, realDestination] = await Promise.all([canonicalFuture(source), canonicalFuture(destination)]);
  for (const [a, b] of [[source, destination], [realSource, realDestination]] as const) {
    if (contains(a, b) || contains(b, a)) {
      throw new Error('Source and destination must be different paths without an ancestor/descendant relationship.');
    }
  }
  const sourceStat = await fs.stat(source);
  if (await exists(destination)) {
    const destinationStat = await fs.stat(destination);
    if (sourceStat.dev === destinationStat.dev && sourceStat.ino === destinationStat.ino) {
      throw new Error('Source and destination refer to the same file.');
    }
    if (!overwrite) throw new Error('Destination exists: ' + destination);
  }
  if (createParents) await fs.mkdir(path.dirname(destination), { recursive: true });
  const transaction = await fs.mkdtemp(path.join(path.dirname(destination), '.chatx-move-'));
  const staged = path.join(transaction, 'source');
  const backup = path.join(transaction, 'previous');
  let renamedSource = false;
  let backedUp = false;
  let committed = false;
  try {
    try {
      await fs.rename(source, staged);
      renamedSource = true;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EXDEV') throw error;
      await fs.cp(source, staged, { recursive: true, force: false, errorOnExist: true });
    }
    // Recheck after staging. Never delete the old destination before commit.
    if (await exists(destination)) {
      if (!overwrite) throw new Error('Destination exists: ' + destination);
      await fs.rename(destination, backup);
      backedUp = true;
    }
    await fs.rename(staged, destination);
    committed = true;
  } catch (error) {
    try {
      if (backedUp) await fs.rename(backup, destination);
      if (renamedSource) await fs.rename(staged, source);
      await fs.rm(transaction, { recursive: true, force: true });
    } catch (rollbackError) {
      throw new Error('Move failed; recovery data retained at ' + transaction + ': ' +
        String(error) + '; rollback: ' + String(rollbackError));
    }
    throw error;
  }
  if (committed) {
    try {
      if (!renamedSource) await fs.rm(source, { recursive: true, force: false });
      await fs.rm(transaction, { recursive: true, force: true });
    } catch (error) {
      // The complete destination is committed. Do not roll back a partially removed source.
      return { moved: true, cleanup_warning: String(error), recovery_directory: transaction };
    }
  }
  return { moved: true };
}

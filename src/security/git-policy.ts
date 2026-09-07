import fs from 'node:fs/promises';
import path from 'node:path';
import { assertExistingPath, assertPathAllowed } from './path-policy.js';

export function gitEnvironment(): NodeJS.ProcessEnv {
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^GIT_/i.test(key)));
  return {
    ...env, GIT_TERMINAL_PROMPT: '0', GIT_OPTIONAL_LOCKS: '0', GIT_NO_LAZY_FETCH: '1',
    GIT_LITERAL_PATHSPECS: '1', GIT_ATTR_NOSYSTEM: '1',
  };
}

export const restrictedGitOptions = [
  '--no-pager',
  '-c', 'core.hooksPath=/dev/null',
  '-c', 'core.fsmonitor=false',
  '-c', 'core.attributesFile=/dev/null',
  '-c', 'core.untrackedCache=false',
  '-c', 'submodule.recurse=false',
  '-c', 'diff.ignoreSubmodules=all',
  '-c', 'status.submoduleSummary=false',
  '-c', 'commit.gpgSign=false',
  '-c', 'tag.gpgSign=false',
  '-c', 'log.showSignature=false',
  '-c', 'maintenance.auto=false',
  '-c', 'gc.auto=0',
  '-c', 'protocol.allow=never',
];

type ProbeResult = { exit_code: number | null; stdout: string; stderr: string; stdout_dropped_chars: number };
type Probe = (cwd: string, args: string[]) => Promise<ProbeResult>;

function complete(result: ProbeResult): string {
  if (result.exit_code !== 0 || result.stdout_dropped_chars) {
    throw new Error(result.stderr.trim() || 'Git repository validation failed or exceeded its output limit.');
  }
  return result.stdout.trim();
}

export async function validateGitRepo(input: string, probe: Probe): Promise<string> {
  const cwd = await assertExistingPath(input);
  if (!(await fs.stat(cwd)).isDirectory()) throw new Error('Repository path is not a directory.');
  const layout = complete(await probe(cwd, [
    'rev-parse', '--path-format=absolute', '--show-toplevel', '--absolute-git-dir', '--git-common-dir',
  ])).split(/\r?\n/);
  if (layout.length !== 3 || layout.some(value => !path.isAbsolute(value))) {
    throw new Error('A Git working tree with unambiguous metadata paths is required.');
  }
  const [root, gitDir, commonDir] = layout as [string, string, string];
  // Authorizing one subdirectory does not authorize its parent repository.
  for (const location of [root, gitDir, commonDir]) await assertExistingPath(location);
  for (const directory of new Set([gitDir, commonDir])) {
    for (const name of ['config', 'config.worktree', 'HEAD', 'index', 'objects', 'refs', 'packed-refs']) {
      await assertPathAllowed(path.join(directory, name));
    }
    const alternates = path.join(directory, 'objects', 'info', 'alternates');
    await assertPathAllowed(alternates);
    try {
      if ((await fs.readFile(alternates, 'utf8')).trim()) {
        throw new Error('Alternate object databases require Advanced Git.');
      }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
  }
  const includes = await probe(cwd, ['config', '--local', '--no-includes', '--name-only',
    '--get-regexp', '^(include|includeif)\\.']);
  if (includes.exit_code !== 1) {
    if (includes.exit_code !== 0) complete(includes);
    throw new Error('Repository config includes require Advanced Git.');
  }
  return cwd;
}

export async function validateGitPaths(cwd: string, values?: string[]): Promise<string[]> {
  if (!values) return [];
  const validated: string[] = [];
  for (const value of values) {
    if (!value || path.isAbsolute(value) || /^[a-z]:/i.test(value) ||
        /[\u0000-\u001f\u007f]/.test(value) || value.startsWith(':') ||
        value.split(/[\\/]/).some(part => part === '..' || part.toLowerCase() === '.git')) {
      throw new Error('Git paths must be literal relative paths without traversal, magic, or .git metadata.');
    }
    await assertPathAllowed(path.resolve(cwd, value));
    validated.push(value);
  }
  return validated;
}

export async function disabledFilterOptions(cwd: string, probe: Probe): Promise<string[]> {
  const result = await probe(cwd, ['config', '--name-only', '--get-regexp',
    '^filter\\..*\\.(clean|smudge|process|required)$']);
  if (result.exit_code === 1) return [];
  const names = complete(result).split(/\r?\n/);
  const drivers = new Set(names.map(name => name.slice(0, name.lastIndexOf('.'))));
  // Fail rather than silently stage unfiltered contents when a filter is required.
  return [...drivers].flatMap(driver => [
    '-c', driver + '.clean=', '-c', driver + '.smudge=',
    '-c', driver + '.process=', '-c', driver + '.required=true',
  ]);
}

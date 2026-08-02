import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

// Overlays are the authoring surface; the derived base-relative diff is the
// canonical record.
export function applyOverlay(tutorial, stepId, workDir) {
  const overlayDir = path.join(tutorial.dir, 'steps', stepId);
  const files = walk(overlayDir);
  const applied = [];
  for (const rel of files) {
    const src = path.join(overlayDir, rel);
    const dest = path.join(workDir, rel);
    const before = fs.existsSync(dest) ? fs.readFileSync(dest, 'utf8') : null;
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    fs.copyFileSync(src, dest);
    applied.push({ file: rel, diff: unifiedDiff(before, fs.readFileSync(dest, 'utf8'), rel) });
  }
  return applied;
}

export function applyJsonMerge(mutation, workDir) {
  const target = path.join(workDir, mutation.file);
  const before = fs.readFileSync(target, 'utf8');
  const merged = deepMerge(JSON.parse(before), mutation.merge);
  const after = JSON.stringify(merged, null, 2) + '\n';
  fs.writeFileSync(target, after);
  return [{ file: mutation.file, diff: unifiedDiff(before, after, mutation.file) }];
}

export function applyShell(mutation, workDir) {
  const cwd = mutation.cwd ? path.join(workDir, mutation.cwd) : workDir;
  execFileSync(mutation.run, { cwd, shell: true, stdio: 'pipe' });
  return [{ command: mutation.run, cwd: mutation.cwd ?? '.' }];
}

// objects merge recursively; arrays are a union (append missing entries) — the
// capabilities-permissions case this exists for
function deepMerge(base, patch) {
  if (Array.isArray(base) && Array.isArray(patch)) {
    const out = [...base];
    const seen = new Set(base.map((v) => JSON.stringify(v)));
    for (const v of patch) if (!seen.has(JSON.stringify(v))) out.push(v);
    return out;
  }
  if (isObject(base) && isObject(patch)) {
    const out = { ...base };
    for (const [k, v] of Object.entries(patch)) {
      out[k] = k in base ? deepMerge(base[k], v) : v;
    }
    return out;
  }
  return patch;
}

const isObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);

function walk(dir, prefix = '') {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) out.push(...walk(path.join(dir, entry.name), rel));
    else out.push(rel);
  }
  return out;
}

function unifiedDiff(before, after, label) {
  if (before === after) return '';
  if (before === null) return `new file: ${label}\n+${after.split('\n').join('\n+')}`;
  // git does the real work; --no-index exits 1 on difference, which is not an error here
  const tmp = fs.mkdtempSync(path.join(fs.realpathSync(require_os_tmpdir()), 'tatu-diff-'));
  try {
    const a = path.join(tmp, 'a');
    const b = path.join(tmp, 'b');
    fs.writeFileSync(a, before);
    fs.writeFileSync(b, after);
    try {
      execFileSync('git', ['diff', '--no-index', '--unified=3', '--', a, b], { stdio: 'pipe' });
      return '';
    } catch (e) {
      const raw = e.stdout?.toString() ?? '';
      return raw
        .split('\n')
        .filter((l) => !l.startsWith('diff --git') && !l.startsWith('index '))
        .map((l) => (l.startsWith('--- ') ? `--- a/${label}` : l.startsWith('+++ ') ? `+++ b/${label}` : l))
        .join('\n');
    }
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

function require_os_tmpdir() {
  return process.env.TMPDIR ?? process.env.TEMP ?? process.env.TMP ?? '/tmp';
}

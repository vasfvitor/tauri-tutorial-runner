import fs from 'node:fs';
import path from 'node:path';
import { parse } from 'yaml';

const MUTATION_ENGINES = new Set(['overlay', 'json-merge', 'shell']);
const ASSERTION_KINDS = new Set(['ipc-local', 'ipc-acl', 'cargo-test', 'shell', 'build', 'e2e']);
// implemented in v0; the rest parse but are skipped with a warning
const IMPLEMENTED_KINDS = new Set(['ipc-local', 'ipc-acl', 'shell']);

export function loadTutorial(dir) {
  const yamlPath = path.join(dir, 'tutorial.yaml');
  if (!fs.existsSync(yamlPath)) throw new Error(`no tutorial.yaml in ${dir}`);
  const doc = parse(fs.readFileSync(yamlPath, 'utf8'));

  for (const key of ['id', 'title', 'base', 'steps']) {
    if (!doc[key]) throw new Error(`tutorial.yaml: missing "${key}"`);
  }
  if (!doc.base.fixture) {
    throw new Error('tutorial.yaml: v0 requires base.fixture (a vendored scaffold dir); base.setup is not implemented yet');
  }
  const fixtureDir = path.join(dir, doc.base.fixture);
  if (!fs.existsSync(fixtureDir)) throw new Error(`base.fixture dir not found: ${fixtureDir}`);

  const seen = new Set();
  for (const step of doc.steps) {
    if (!step.id) throw new Error('tutorial.yaml: every step needs an id');
    if (seen.has(step.id)) throw new Error(`tutorial.yaml: duplicate step id "${step.id}"`);
    seen.add(step.id);
    if (!step.task) throw new Error(`step "${step.id}": missing task (the agent-eval prompt)`);

    for (const m of step.mutations ?? []) {
      if (!MUTATION_ENGINES.has(m.engine)) {
        throw new Error(`step "${step.id}": unknown mutation engine "${m.engine}"`);
      }
      if (m.engine === 'overlay') {
        const overlayDir = path.join(dir, 'steps', step.id);
        if (!fs.existsSync(overlayDir)) {
          throw new Error(`step "${step.id}": overlay mutation but no steps/${step.id}/ dir`);
        }
      }
      if (m.engine === 'json-merge' && (!m.file || !m.merge)) {
        throw new Error(`step "${step.id}": json-merge needs file + merge`);
      }
      if (m.engine === 'shell' && !m.run) {
        throw new Error(`step "${step.id}": shell mutation needs run`);
      }
    }

    for (const a of [...(step.preconditions ?? []), ...(step.assertions ?? [])]) {
      if (!ASSERTION_KINDS.has(a.kind)) {
        throw new Error(`step "${step.id}": unknown assertion kind "${a.kind}"`);
      }
      // parse-time guard: mock-context-shaped assertions can
      // never pass for plugin commands, so reject the combination outright
      if (a.kind === 'ipc-local' && String(a.command ?? '').startsWith('plugin:')) {
        throw new Error(
          `step "${step.id}": ipc-local cannot target "${a.command}" — plugin commands need ipc-acl (resolved capability ACL)`
        );
      }
      if ((a.kind === 'ipc-local' || a.kind === 'ipc-acl') && !a.command) {
        throw new Error(`step "${step.id}": ${a.kind} needs command`);
      }
      if ((a.kind === 'ipc-local' || a.kind === 'ipc-acl') && a.expect === undefined) {
        throw new Error(`step "${step.id}": ${a.kind} needs expect (ok / ok_contains / denied)`);
      }
    }
  }

  return { ...doc, dir, fixtureDir };
}

export { IMPLEMENTED_KINDS };

import fs from 'node:fs';
import path from 'node:path';
import { spawnSync, execFileSync } from 'node:child_process';
import { applyOverlay, applyJsonMerge, applyShell } from './mutations.js';
import { ensureHarness, generateIpcTestFile, sanitize } from './harness.js';
import { IMPLEMENTED_KINDS } from './tutorial.js';

export async function runTutorial(tutorial, { authoritative, onlyStep }) {
  const repoRoot = path.resolve(tutorial.dir, '..', '..');
  const workDir = path.join(repoRoot, '.tatu', 'work', tutorial.id);
  // shared across runs so a re-check pays incremental compile cost, not a cold build
  const targetDir = path.join(repoRoot, '.tatu', 'cargo-target', tutorial.id);

  fs.rmSync(workDir, { recursive: true, force: true });
  fs.mkdirSync(workDir, { recursive: true });
  fs.cpSync(tutorial.fixtureDir, workDir, { recursive: true });
  console.log(`work tree: ${workDir}`);

  const manifest = {
    id: tutorial.id,
    title: tutorial.title,
    advisory: !authoritative,
    platform: process.platform,
    steps: [],
  };

  let failed = false;

  for (const step of tutorial.steps) {
    if (onlyStep && step.id !== onlyStep) continue;
    console.log(`\n== step: ${step.id}`);
    const record = { id: step.id, task: step.task, mutations: [], preconditions: [], assertions: [] };
    manifest.steps.push(record);

    record.preconditions = runAssertionPhase(tutorial, step, step.preconditions ?? [], 'pre', workDir, targetDir);

    for (const m of step.mutations ?? []) {
      if (m.engine === 'overlay') record.mutations.push(...applyOverlay(tutorial, step.id, workDir));
      else if (m.engine === 'json-merge') record.mutations.push(...applyJsonMerge(m, workDir));
      else if (m.engine === 'shell') record.mutations.push(...applyShell(m, workDir));
    }
    if (record.mutations.length) console.log(`   applied ${record.mutations.length} mutation(s)`);

    record.assertions = runAssertionPhase(tutorial, step, step.assertions ?? [], 'step', workDir, targetDir);

    const bad = [...record.preconditions, ...record.assertions].filter((r) => r.status === 'fail');
    if (bad.length) {
      failed = true;
      console.log(`   step ${step.id}: FAIL`);
      break; // later steps build on this state; no point continuing
    }
    console.log(`   step ${step.id}: ok`);
  }

  const outDir = path.join(repoRoot, '.tatu', 'out', tutorial.id);
  fs.mkdirSync(outDir, { recursive: true });
  const manifestPath = path.join(outDir, 'tutorial.manifest.json');
  fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n');
  console.log(`\nmanifest: ${manifestPath}${manifest.advisory ? ' (advisory)' : ''}`);

  if (failed) throw new Error('tutorial failed — see output above');
}

function runAssertionPhase(tutorial, step, list, phase, workDir, targetDir) {
  const results = [];
  if (!list.length) return results;

  const skipped = list.filter((a) => !IMPLEMENTED_KINDS.has(a.kind));
  for (const a of skipped) {
    console.log(`   skip (${a.kind} not implemented in v0)`);
    results.push({ kind: a.kind, status: 'skipped' });
  }

  const ipc = list.filter((a) => a.kind === 'ipc-local' || a.kind === 'ipc-acl');
  if (ipc.length) {
    ensureHarness(workDir);
    const testName = `tatu_${phase}_${sanitize(step.id)}`;
    const harness = step.harness ?? tutorial.harness ?? {};
    const source = generateIpcTestFile({
      name: testName,
      prelude: harness.prelude,
      plugins: harness.plugins,
      handlers: harness.handlers,
      cases: ipc.map((a, i) => ({
        label: `${phase}_${i}_${sanitize(a.command)}`,
        command: a.command,
        args: a.args,
        expect: a.expect,
        setup: a.setup,
      })),
    });
    fs.writeFileSync(path.join(workDir, 'src-tauri', 'tests', `${testName}.rs`), source);

    const res = cargoTest(workDir, targetDir, testName);
    for (const a of ipc) {
      results.push({ kind: a.kind, command: a.command, status: res.ok ? 'pass' : 'fail' });
    }
    if (!res.ok) console.log(indentOutput(res.output));
    else console.log(`   ${testName}: pass (${res.seconds}s)`);
  }

  for (const a of list.filter((x) => x.kind === 'shell')) {
    const r = spawnSync(a.run, { cwd: workDir, shell: true, stdio: 'pipe', encoding: 'utf8' });
    const ok = r.status === 0;
    results.push({ kind: 'shell', run: a.run, status: ok ? 'pass' : 'fail' });
    if (!ok) console.log(indentOutput(`${r.stdout}\n${r.stderr}`));
  }

  return results;
}

function cargoTest(workDir, targetDir, testName) {
  const started = Date.now();
  const r = spawnSync('cargo', ['test', '--test', testName], {
    cwd: path.join(workDir, 'src-tauri'),
    env: { ...process.env, CARGO_TARGET_DIR: targetDir },
    stdio: 'pipe',
    encoding: 'utf8',
    timeout: 15 * 60 * 1000,
  });
  return {
    ok: r.status === 0,
    output: `${r.stdout ?? ''}\n${r.stderr ?? ''}`,
    seconds: Math.round((Date.now() - started) / 1000),
  };
}

function indentOutput(text) {
  return text
    .split('\n')
    .slice(-40)
    .map((l) => `   | ${l}`)
    .join('\n');
}

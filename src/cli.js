import path from 'node:path';
import { loadTutorial } from './tutorial.js';
import { runTutorial } from './runner.js';

const USAGE = `tatu — executable, verifiable Tauri tutorials

Usage:
  tatu check <tutorial-dir> [--step <id>]   advisory run on the host; never authoritative
  tatu run   <tutorial-dir> [--step <id>]   authoritative run (container); writes the manifest
  tatu validate <tutorial-dir>              parse + validate tutorial.yaml only

check runs on whatever toolchain the host has (results are advisory: different
platform = different substrate). run refuses to write a manifest outside the
pinned container unless TATU_ALLOW_LOCAL=1 is set explicitly.`;

export async function main(argv) {
  const [command, ...rest] = argv;
  if (!command || command === '--help' || command === '-h') {
    console.log(USAGE);
    return;
  }

  const dirArg = rest.find((a) => !a.startsWith('--'));
  if (!dirArg) throw new Error(`missing <tutorial-dir>\n\n${USAGE}`);
  const dir = path.resolve(dirArg);

  const stepFlag = rest.includes('--step') ? rest[rest.indexOf('--step') + 1] : null;

  const tutorial = loadTutorial(dir);
  console.log(`loaded ${tutorial.id}: "${tutorial.title}" (${tutorial.steps.length} steps)`);

  if (command === 'validate') return;

  if (command === 'check' || command === 'run') {
    const authoritative = command === 'run';
    if (authoritative && !process.env.TATU_CONTAINER && process.env.TATU_ALLOW_LOCAL !== '1') {
      throw new Error(
        'run is container-only (authoritative). Use `tatu check` on the host, or set TATU_ALLOW_LOCAL=1 to override.'
      );
    }
    await runTutorial(tutorial, { authoritative, onlyStep: stepFlag });
    return;
  }

  throw new Error(`unknown command "${command}"\n\n${USAGE}`);
}

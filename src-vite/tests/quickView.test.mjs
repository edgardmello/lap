import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';

const source = await readFile(new URL('../src/common/quickView.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } });
const { mergeQuickViewFiles, selectQuickViewFile } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'));
const file = path => ({ path, name: path.split('/').pop(), size: 1 });

test('repeated drops append only new originals, preserving existing order and selection identity', () => {
  const existing = [file('/a.jpg'), file('/b.jpg')];
  const result = mergeQuickViewFiles(existing, [file('/b.jpg'), file('/c.jpg'), file('/c.jpg')]);
  assert.deepEqual(result.files.map(x => x.path), ['/a.jpg', '/b.jpg', '/c.jpg']);
  assert.equal(result.files[0], existing[0]);
  assert.equal(existing.length, 2);
  assert.equal(result.truncated, false);
});
test('total session bound holds across drops and duplicate files do not consume capacity', () => {
  const full = Array.from({ length: 1000 }, (_, i) => file(`/image-${i}.jpg`));
  assert.equal(mergeQuickViewFiles(full, full).truncated, false);
  const result = mergeQuickViewFiles(full, [file('/new.jpg')]);
  assert.equal(result.files.length, 1000);
  assert.equal(result.truncated, true);
});
test('plain selection replaces, modifier toggles and Shift selects both forward and reverse ranges', () => {
  const paths = ['a', 'b', 'c', 'd'];
  assert.deepEqual([...selectQuickViewFile(paths, new Set(['b']), 'c', 'b', false, false)], ['c']);
  assert.deepEqual([...selectQuickViewFile(paths, new Set(['b']), 'c', 'b', true, false)], ['b', 'c']);
  assert.deepEqual([...selectQuickViewFile(paths, new Set(['b']), 'b', 'b', true, false)], []);
  assert.deepEqual([...selectQuickViewFile(paths, new Set(['d']), 'c', 'a', false, true)], ['a', 'b', 'c']);
  assert.deepEqual([...selectQuickViewFile(paths, new Set(), 'a', 'c', false, true)], ['a', 'b', 'c']);
  assert.deepEqual([...selectQuickViewFile(paths, new Set(['d']), 'b', 'missing', false, true)], ['b']);
});

import { readFileSync, readdirSync } from 'node:fs';
import { extname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const repositoryRoot = fileURLToPath(new URL('..', import.meta.url));
const packageRoot = join(repositoryRoot, 'packages', 'sdk');
const distRoot = join(packageRoot, 'dist');
const packageJson = JSON.parse(readFileSync(join(packageRoot, 'package.json'), 'utf8'));

const files = walk(distRoot).sort();
const relativeFiles = files.map((file) => relative(distRoot, file));
const declarationFiles = relativeFiles.filter((file) => file.endsWith('.d.ts'));
const expectedDeclarations = ['client.d.ts', 'index.d.ts', 'observation.d.ts', 'react.d.ts'];

if (JSON.stringify(declarationFiles) !== JSON.stringify(expectedDeclarations)) {
  fail(
    `published declarations must be exactly ${expectedDeclarations.join(', ')}; found ${declarationFiles.join(', ')}`,
  );
}

const forbidden = [
  ['legacy oracle module', /legacy-(?:api|oracle)/i],
  ['TypeScript SQLite owner', /\bSqliteService\b/],
  ['legacy live owner', /\bSpaghettiLive\b/],
  ['SQLite JavaScript binding', /better-sqlite3|node:sqlite/],
  ['legacy storage graph', /(?:data\/sqlite|live\/watcher|createSpaghetti)/],
  ['embedded schema DDL', /\bCREATE\s+TABLE\b/i],
];

for (const file of files) {
  if (!['.js', '.cjs', '.ts'].includes(extname(file))) continue;
  const content = readFileSync(file, 'utf8');
  for (const [label, pattern] of forbidden) {
    if (pattern.test(content)) fail(`${label} leaked into ${relative(distRoot, file)}`);
  }
  if (file.endsWith('.d.ts') && /\bfrom\s+['"]\.\.?\//.test(content)) {
    fail(`unrolled relative declaration import remains in ${relative(distRoot, file)}`);
  }
}

for (const [subpath, targets] of Object.entries(packageJson.exports)) {
  for (const [condition, target] of Object.entries(targets)) {
    const normalized = target.replace(/^\.\//, '');
    if (!relativeFiles.includes(normalized.replace(/^dist\//, ''))) {
      fail(`package export ${subpath} (${condition}) points at missing ${target}`);
    }
  }
}

console.log(
  `SDK package boundary verified: ${relativeFiles.length} files, ${declarationFiles.length} rolled declarations.`,
);

function walk(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? walk(path) : [path];
  });
}

function fail(message) {
  console.error(`SDK package boundary failed: ${message}`);
  process.exit(1);
}

/**
 * Integration test — Verify cold start correctly parses all projects with JSONL files.
 *
 * ⚠️  DANGER: this manual dev script ingests into your REAL Spaghetti cache DB at
 *     ~/.spaghetti/cache/spaghetti.db (createSpaghettiService() uses the default
 *     path). Unlike the other dev scripts there is no local DB-path constant to
 *     redirect — the path is chosen inside the SDK — so run it only when you
 *     intend to touch real data.
 *
 * Usage: rm -f ~/.spaghetti/cache/spaghetti.db && npx tsx scripts/test-cold-start.ts
 */

import { createSpaghettiService } from '../packages/sdk/src/legacy-oracle.js';

async function main() {
  console.log('Creating service...');
  const spaghetti = createSpaghettiService();

  spaghetti.on('progress', (p: { phase: string; message: string; current?: number; total?: number }) => {
    if (p.current !== undefined) {
      console.log(`[progress] ${p.phase}: ${p.message} (${p.current}/${p.total})`);
    } else {
      console.log(`[progress] ${p.phase}: ${p.message}`);
    }
  });

  console.log('Initializing (cold start)...');
  await spaghetti.initialize();
  console.log('Ready!');

  const projects = spaghetti.getProjectList();
  console.log(`\nTotal projects: ${projects.length}`);

  // Check for projects with sessions but 0 messages
  let zeroMsgWithJsonl = 0;
  let zeroMsgTotal = 0;
  const previouslyBroken = ['-Users-jamesyong', '-Users-jamesyong-Projects-project100-p008'];

  for (const p of projects) {
    if (p.sessionCount > 0 && p.messageCount === 0) {
      zeroMsgTotal++;
      // Note: we can't easily check has_jsonl from here, just report
      console.log(`  ZERO msgs: ${p.slug} (sessions=${p.sessionCount})`);
    }
  }

  console.log(`\nProjects with sessions but 0 messages: ${zeroMsgTotal}`);

  for (const slug of previouslyBroken) {
    const p = projects.find((x: { slug: string }) => x.slug === slug);
    if (p) {
      const status = p.messageCount > 0 ? 'FIXED' : 'STILL BROKEN';
      console.log(`  ${status}: ${p.slug} sessions=${p.sessionCount} messages=${p.messageCount}`);
    } else {
      console.log(`  NOT FOUND: ${slug}`);
    }
  }

  spaghetti.shutdown();
  console.log('\nDone.');
}

main().catch(console.error);

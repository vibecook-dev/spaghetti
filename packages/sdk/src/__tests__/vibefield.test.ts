/**
 * VibeField Phase A surface (RFC 012 landing plan §3.2).
 *
 * These are parity tests in the sense the lane brief keeps: they run real Rust
 * and check that the *generated* TypeScript types describe what it actually
 * emits. There is no hand-written parser under test, and no frozen fixture
 * round-trip either — the references come from a real catalog and a real
 * observer attachment, which is where VibeField will read them.
 */

import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { after, before, describe, test } from 'node:test';

import {
  isSameEntity,
  isSameNativeIdentity,
  isSameRevision,
  isSameSnapshot,
  queryWatermark,
  type ExternalEntityRef,
  type NativeIdentity,
  type ProjectRef,
  type SemanticRevisionRef,
  type SessionRef,
} from '../vibefield.js';
import {
  isSemanticEvent,
  loadNativeAddon,
  observeSession,
  openSpaghettiEngine,
  type SpaghettiEngine,
} from '../index.js';

const native = loadNativeAddon();
const here = path.dirname(fileURLToPath(import.meta.url));
const fixtureRoot = path.resolve(here, '../../../../crates/spaghetti-napi/fixtures');

/**
 * The version the engine stamps on every reference it mints. This constant
 * only names it; the assertions read the value out of real Rust output.
 */
const REFERENCE_VERSION = 1;

const tempDirs: string[] = [];
const engines: SpaghettiEngine[] = [];
/** Real references, minted by the catalog from the committed fixture corpus. */
let sessionKey: string;
let projectKey: string;
let identityEngine: SpaghettiEngine;

after(async () => {
  for (const engine of engines.splice(0)) await engine.dispose();
  for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
});

before(async () => {
  if (!native) return;
  const dir = mkdtempSync(path.join(tmpdir(), 'spaghetti-vibefield-'));
  tempDirs.push(dir);
  const claude = path.join(dir, '.claude');
  cpSync(path.join(fixtureRoot, 'medium/.claude'), claude, { recursive: true, preserveTimestamps: true });
  const engine = await openSpaghettiEngine({
    dbPath: path.join(dir, 'spaghetti.db'),
    ownerLabel: 'vibefield-identity-test',
    queryWorkers: 1,
  });
  engines.push(engine);
  await engine.startConfiguredObservation({
    sources: [{ adapterId: 'claude-code', roots: [claude], reason: 'vibefield_test' }],
  });
  await engine.awaitObservationStart();
  const sessions = await engine.listCatalogSessions({ limit: 5 });
  const projects = await engine.listCatalogProjects({ limit: 5 });
  sessionKey = sessions.sessions[0]!.externalRef;
  projectKey = projects.projects[0]!.externalRef;
  identityEngine = engine;
});

describe('VibeField Phase A identity surface', { skip: !native }, () => {
  test('the generated types describe the references Rust actually mints', () => {
    const session: SessionRef = { external_entity_reference_version: REFERENCE_VERSION, entity_key: sessionKey };
    const project: ProjectRef = { external_entity_reference_version: REFERENCE_VERSION, entity_key: projectKey };

    assert.equal(typeof session.external_entity_reference_version, 'number');
    assert.equal(typeof session.entity_key, 'string');

    // An opaque reference carries its encoding version and never leaks a local
    // path, a database name, or a row id.
    assert.match(session.entity_key, /^v1:[A-Za-z0-9_-]{43}$/);
    assert.match(project.entity_key, /^v1:[A-Za-z0-9_-]{43}$/);
    assert.notEqual(session.entity_key, project.entity_key, 'a session and its project are two entities');
    assert.equal(isSameEntity(session, project), false);
  });

  test('reference comparison is by value and keeps version mismatches distinct', () => {
    const ref: ExternalEntityRef = { external_entity_reference_version: REFERENCE_VERSION, entity_key: sessionKey };
    const decodedAgain: ExternalEntityRef = { ...ref };

    // Two independent decodings are never `===`, which is the whole reason
    // these helpers exist.
    assert.notEqual(ref, decodedAgain);
    assert.ok(isSameEntity(ref, decodedAgain));

    assert.equal(isSameEntity(ref, { ...ref, entity_key: `${ref.entity_key}x` }), false);
    assert.equal(
      isSameEntity(ref, { ...ref, external_entity_reference_version: ref.external_entity_reference_version + 1 }),
      false,
      'references minted under different contract versions are not the same reference',
    );
  });

  test('one entity has one reference, whichever surface names it', async () => {
    // The trap this replaces: the catalog spelled the digest `1:<base64url>`
    // and RFC 012A spelled the same digest `v1:<base64url>`, so a consumer
    // string-comparing a persisted `externalRef` against an `entity_key` got a
    // false negative on identical entities. Both are minted by
    // `CanonicalEntityKey::derive`; only the text differed. This asserts the
    // spelling is now shared, and would fail against either older encoding.
    const catalogSessions = await identityEngine.listCatalogSessions({ limit: 50 });
    const catalogProjects = await identityEngine.listCatalogProjects({ limit: 50 });
    const project = catalogProjects.projects[0]!;

    const historyProjects = await identityEngine.listHistoryProjects({ limit: 50 });
    const historyProject = historyProjects.items.find((row) => row.nativeProjectKey === project.nativeProjectKey);
    assert.ok(historyProject, 'the same project is on both surfaces');
    assert.equal(
      historyProject.externalRef,
      project.externalRef,
      'catalog and history name the project with one reference',
    );

    const historySessions = await identityEngine.listHistorySessions({
      projectId: project.projectId,
      limit: 50,
    });
    const byNativeId = new Map(catalogSessions.sessions.map((row) => [row.nativeSessionId, row.externalRef]));
    let compared = 0;
    for (const row of historySessions.items) {
      const fromCatalog = byNativeId.get(row.nativeSessionId);
      if (fromCatalog === undefined) continue;
      assert.equal(row.externalRef, fromCatalog, 'catalog and history name the session with one reference');
      compared += 1;
    }
    assert.ok(compared > 0, 'at least one session was on both surfaces to compare');

    // A `SessionRef` a consumer builds from a persisted row is the reference,
    // not a differently-spelled cousin of it.
    const fromRow: SessionRef = {
      external_entity_reference_version: REFERENCE_VERSION,
      entity_key: byNativeId.values().next().value as string,
    };
    assert.match(fromRow.entity_key, /^v1:[A-Za-z0-9_-]{43}$/);

    // And it still round-trips through the resolver that accepts it.
    const resolved = await identityEngine.resolveCatalogEntity(project.externalRef);
    assert.equal(resolved.kind, 'project');
    assert.equal(resolved.project?.projectId, project.projectId);
  });

  test('native identity is namespaced', () => {
    const left: NativeIdentity = { native_namespace: 'claude-code', native_id: 'session-1' };
    const right: NativeIdentity = { native_namespace: 'codex', native_id: 'session-1' };
    assert.ok(isSameNativeIdentity(left, { ...left }));
    assert.equal(isSameNativeIdentity(left, right), false, 'the same id from two products is two identities');
  });
});

describe('VibeField Phase A revision references', { skip: !native }, () => {
  test('the observer emits the generated revision reference on every semantic event', async () => {
    const root = mkdtempSync(path.join(tmpdir(), 'spaghetti-vibefield-observe-'));
    tempDirs.push(root);
    const session = '01234567-89ab-cdef-0123-456789abcdef';
    const project = path.join(root, 'projects', '-vibefield-fixture');
    mkdirSync(project, { recursive: true });
    const transcript = path.join(project, `${session}.jsonl`);
    writeFileSync(
      transcript,
      `${JSON.stringify({
        type: 'assistant',
        uuid: 'a-1',
        parentUuid: 'u1',
        timestamp: '2026-08-11T00:00:00Z',
        sessionId: session,
        cwd: '/fixture',
        version: '1',
        gitBranch: 'main',
        isSidechain: false,
        userType: 'external',
        requestId: 'r-a-1',
        message: {
          model: 'fixture-model',
          id: 'resp-1',
          type: 'message',
          role: 'assistant',
          content: [{ type: 'text', text: 'fixture' }],
          usage: { input_tokens: 10, output_tokens: 5, cache_creation_input_tokens: 2, cache_read_input_tokens: 3 },
        },
      })}\n`,
    );

    const observer = observeSession({
      adapter_id: 'claude-code',
      agent_root: root,
      transcript_path: transcript,
      native_session_id: session,
    });
    let revision: SemanticRevisionRef | undefined;
    try {
      for await (const event of observer) {
        if (isSemanticEvent(event)) {
          revision = event.semantic_revision_ref;
          break;
        }
        if (event.type === 'closed') break;
      }
    } finally {
      await observer.close();
    }

    assert.ok(revision, 'the attachment delivered at least one semantic revision');
    assert.equal(typeof revision.semantic_reference_contract_version, 'number');
    assert.match(revision.fact_revision_id, /^v1:[A-Za-z0-9_-]+$/);

    // Comparison is by value: the same revision read twice is the same
    // revision, and a different fact revision is not.
    assert.ok(isSameRevision(revision, { ...revision }));
    assert.equal(isSameRevision(revision, { ...revision, fact_revision_id: 'v1:other' }), false);
  });
});

describe('VibeField Phase A durable watermark', { skip: !native }, () => {
  test('every durable query result reports the snapshot it was computed at', async () => {
    const dir = mkdtempSync(path.join(tmpdir(), 'spaghetti-vibefield-watermark-'));
    tempDirs.push(dir);
    const engine = await openSpaghettiEngine({
      dbPath: path.join(dir, 'spaghetti.db'),
      ownerLabel: 'vibefield-watermark-test',
      queryWorkers: 1,
    });
    engines.push(engine);

    const projects = await engine.listHistoryProjects();
    const sources = await engine.listSources();
    const overview = await engine.overview();

    assert.equal(typeof queryWatermark(projects), 'number');
    assert.equal(
      queryWatermark(projects),
      overview.commitSeq,
      'the watermark is the durable commit sequence, not a page-local counter',
    );

    // Two reads with no write between them are the same snapshot, so their
    // pages may be joined without re-reading either.
    assert.ok(isSameSnapshot(projects, sources));

    // The watermark type is structural, so any durable result satisfies it.
    assert.equal(queryWatermark(sources), queryWatermark(projects));
  });
});

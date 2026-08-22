import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { parseObservationContractRequest, type ObservationContractRequest } from '../contracts/rfc012d.js';
import type { NativeScopedObservation } from '../native.js';
import {
  ScopedObservationRequestError,
  ScopedObservationTransportError,
  createSessionObserverForTransport,
  encodeScopedObservationRequestForTransport,
} from '../scoped-observation.js';

type JsonObject = Record<string, any>;

function fixture(name: string): JsonObject {
  return JSON.parse(
    readFileSync(new URL(`../../../../crates/spaghetti-napi/fixtures/contracts/${name}`, import.meta.url), 'utf8'),
  ) as JsonObject;
}

const negotiation = fixture('rfc012d-observation-negotiation-v1.json');
const capabilities = fixture('rfc012d-scoped-capability-snapshot-v1.json');
const completion = fixture('rfc012d-scoped-completion-envelope-v1.json');
const watermark = fixture('rfc012d-scoped-watermark-v2.json');

function completionUnion(name: 'bootstrap' | 'resync'): string {
  return JSON.stringify({
    scoped_observation_event_union_contract_version: 1,
    family: 'completion',
    context: completion[name].context,
    event: completion[name].event,
  });
}

class FakeNativeObserver implements NativeScopedObservation {
  readonly events: string[];
  readonly endWhenDrained: boolean;
  acknowledgements = 0;
  closes = 0;
  polls = 0;
  readyOffers = 0;
  resyncOffers = 0;
  capabilityJson = JSON.stringify(capabilities.exact);
  pollJsonValue = JSON.stringify({ context: watermark.context, watermark: watermark.watermark });
  #endWaiter: ((value: null) => void) | undefined;

  constructor(events: string[] = [], endWhenDrained = false) {
    this.events = [...events];
    this.endWhenDrained = endWhenDrained;
  }

  capabilitiesJson(): string {
    return this.capabilityJson;
  }

  async nextEventJson(): Promise<string | null> {
    const event = this.events.shift();
    if (event !== undefined) return event;
    if (this.endWhenDrained) return null;
    return new Promise<null>((resolve) => {
      this.#endWaiter = resolve;
    });
  }

  async acknowledgeApplied(): Promise<void> {
    this.acknowledgements += 1;
  }

  async pollJson(): Promise<string> {
    this.polls += 1;
    return this.pollJsonValue;
  }

  async readyOffered(): Promise<void> {
    this.readyOffers += 1;
  }

  async resyncOffered(): Promise<void> {
    this.resyncOffers += 1;
  }

  async close(): Promise<void> {
    this.closes += 1;
    this.#endWaiter?.(null);
    this.#endWaiter = undefined;
  }
}

test('scoped observer request encoder fixes store-free authority and canonical identities', () => {
  const json = encodeScopedObservationRequestForTransport({
    adapterId: 'claude-code',
    configuredRoots: ['/private/fixture-root'],
    programId: 'session-scope-v1',
    knownObjectRelativePaths: { 'root-transcript': 'projects/session.jsonl' },
    rootIdentity: {
      sessionIdentityKey: new Uint8Array([0, 1, 2, 255]),
      rootRunIdentityKey: null,
      relationIdentityInputs: { 'native-session-id': new TextEncoder().encode('session-1') },
    },
    contractRequest: negotiation.contract_request as ObservationContractRequest,
  });
  const wire = JSON.parse(json) as JsonObject;
  assert.equal(wire.persistence, 'none');
  assert.equal(wire.scope_mode, 'exact_known_objects');
  assert.equal(wire.root_identity.session_identity_key, 'AAEC_w');
  assert.equal(wire.root_identity.root_run_identity_key, null);
  assert.equal(wire.root_identity.relation_identity_inputs['native-session-id'], 'c2Vzc2lvbi0x');
  assert.deepEqual(wire.contract_request, parseObservationContractRequest(negotiation.contract_request));
  assert.ok(Buffer.byteLength(json, 'utf8') <= 256 * 1024);
});

test('scoped observer request failures are closed and never echo attacker input', () => {
  const attacker = '/Users/alice/private/session.jsonl';
  assert.throws(
    () =>
      encodeScopedObservationRequestForTransport({
        adapterId: attacker,
        configuredRoots: [attacker],
        programId: 'session-scope-v1',
        knownObjectRelativePaths: { 'root-transcript': attacker },
        rootIdentity: {
          sessionIdentityKey: new Uint8Array([1]),
          relationIdentityInputs: { 'native-session-id': new Uint8Array([1]) },
        },
        contractRequest: negotiation.contract_request as ObservationContractRequest,
      }),
    (error: unknown) => {
      assert.ok(error instanceof ScopedObservationRequestError);
      assert.equal(error.message, 'invalid scoped observation request');
      assert.doesNotMatch(error.message, /Users|alice|private|session\.jsonl/);
      return true;
    },
  );
  const unsupported = structuredClone(negotiation.contract_request) as JsonObject;
  unsupported.contract_versions.fact_family_versions = { 'runtime.message': [1] };
  assert.throws(
    () =>
      encodeScopedObservationRequestForTransport({
        adapterId: 'claude-code',
        configuredRoots: ['/private/fixture-root'],
        programId: 'session-scope-v1',
        knownObjectRelativePaths: { 'root-transcript': 'projects/session.jsonl' },
        rootIdentity: {
          sessionIdentityKey: new Uint8Array([1]),
          relationIdentityInputs: { 'native-session-id': new Uint8Array([1]) },
        },
        contractRequest: unsupported as ObservationContractRequest,
      }),
    ScopedObservationRequestError,
  );
});

test('consumer readiness advances only after the completion delivery is applied', async () => {
  const native = new FakeNativeObserver([completionUnion('bootstrap')]);
  const observer = createSessionObserverForTransport(native);
  const ready = observer.ready();
  assert.ok(Object.isFrozen(observer.capabilities()));
  let applied = 0;
  const consumption = observer.consume(async (event) => {
    assert.equal(event.family, 'completion');
    assert.ok(Object.isFrozen(event));
    assert.ok(Object.isFrozen(event.event));
    applied += 1;
  });
  const barrier = await ready;
  await observer.close();
  await consumption;
  assert.equal(barrier.scope_epoch, 1);
  assert.equal(applied, 1);
  assert.equal(native.acknowledgements, 1);
  assert.equal(native.readyOffers, 1);
  assert.equal(native.closes, 1);
});

test('application failure leaves the receipt unacknowledged and closes the owner', async () => {
  const native = new FakeNativeObserver([completionUnion('bootstrap')]);
  const observer = createSessionObserverForTransport(native);
  await assert.rejects(
    observer.consume(async () => {
      throw new Error('consumer reducer failed');
    }),
    /consumer reducer failed/,
  );
  assert.equal(native.acknowledgements, 0);
  assert.equal(native.closes, 1);
});

test('unexpected native stream termination fails closed', async () => {
  const native = new FakeNativeObserver([], true);
  const observer = createSessionObserverForTransport(native);
  await assert.rejects(
    observer.consume(async () => undefined),
    (error: unknown) =>
      error instanceof ScopedObservationTransportError &&
      error.message === 'native scoped observation ended unexpectedly',
  );
  assert.equal(native.acknowledgements, 0);
  assert.equal(native.closes, 1);
});

test('poll and explicit resync consume only strict contextual native values', async () => {
  const native = new FakeNativeObserver([completionUnion('bootstrap'), completionUnion('resync')]);
  const observer = createSessionObserverForTransport(native);
  const poll = await observer.poll();
  assert.equal(poll.scope_epoch, watermark.watermark.scope_epoch);
  assert.ok(Object.isFrozen(poll));
  assert.equal(native.polls, 1);

  const replacement = observer.resync();
  const consumption = observer.consume(async () => undefined);
  const barrier = await replacement;
  await observer.close();
  await consumption;
  assert.equal(barrier.replacement, 'full_snapshot');
  assert.equal(native.resyncOffers, 1);
  assert.equal(native.acknowledgements, 2);
});

test('native transport drift fails before a typed observer can be returned', () => {
  const native = new FakeNativeObserver();
  native.capabilityJson = JSON.stringify({ ...capabilities.exact, future: true });
  assert.throws(
    () => createSessionObserverForTransport(native),
    (error: unknown) => error instanceof ScopedObservationTransportError,
  );
});

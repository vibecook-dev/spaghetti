import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import { parseScopedCloseContext, parseScopedCloseReceipt } from '../rfc012d-close.js';

interface CloseFixture {
  context: Record<string, any>;
  receipt: Record<string, any>;
}

const fixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-close-v1.json', import.meta.url),
    'utf8',
  ),
) as CloseFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, context: unknown = fixture.context): void {
  assert.throws(() => parseScopedCloseReceipt(value, context), ContractValidationError);
}

test('portable TypeScript independently parses the Rust close receipt', () => {
  const context = parseScopedCloseContext(fixture.context);
  assert.deepEqual(parseScopedCloseReceipt(fixture.receipt, context), fixture.receipt);
  assert.equal(fixture.receipt.outcome, 'closed');
  for (const forbidden of [
    'active_operations',
    'active_watcher_tasks',
    'consumer_drain_pending',
    'applied_through_sequence',
    'observed_at',
  ]) {
    assert.equal(Object.hasOwn(fixture.receipt, forbidden), false);
  }
});

test('selection, full root, attachment, and request identities are caller-held', () => {
  const selection = clone(fixture.receipt);
  selection.contract_selection.lifecycle_contract_version = 2;
  reject(selection);

  for (const field of ['adapter_id', 'source_instance_key', 'session_key', 'root_actor_run_key']) {
    const root = clone(fixture.receipt);
    root.root[field] = field === 'adapter_id' ? 'other' : fixture.receipt.attachment_ref;
    reject(root);
  }

  const attachment = clone(fixture.receipt);
  attachment.attachment_ref = fixture.receipt.close_request_id;
  reject(attachment);

  const request = clone(fixture.receipt);
  request.close_request_id = fixture.receipt.attachment_ref;
  reject(request);

  const foreignContext = clone(fixture.context);
  foreignContext.attachment_ref = fixture.receipt.close_request_id;
  reject(fixture.receipt, foreignContext);
});

test('close receipt and nested root shapes are strict', () => {
  const extra = clone(fixture.receipt);
  extra.future = true;
  reject(extra);

  const missing = clone(fixture.receipt);
  delete missing.close_request_id;
  reject(missing);

  const rootExtra = clone(fixture.receipt);
  rootExtra.root.native_path = '/private/session';
  reject(rootExtra);

  const claimExtra = clone(fixture.receipt);
  claimExtra.root.native_session_claim.future = true;
  reject(claimExtra);

  const missingClaim = clone(fixture.receipt);
  delete missingClaim.root.native_session_claim;
  reject(missingClaim);

  const prototype = Object.assign(Object.create({ inherited: true }), fixture.receipt);
  reject(prototype);
});

test('only a canonical completed acknowledgement is accepted', () => {
  const version = clone(fixture.receipt);
  version.scoped_close_receipt_contract_version = 2;
  reject(version);

  const outcome = clone(fixture.receipt);
  outcome.outcome = 'closing';
  reject(outcome);

  const padded = clone(fixture.receipt);
  padded.attachment_ref += '=';
  reject(padded);

  const short = clone(fixture.receipt);
  short.close_request_id = 'v1:AA';
  reject(short);

  const noncanonical = clone(fixture.receipt);
  noncanonical.attachment_ref = `${noncanonical.attachment_ref.slice(0, -1)}1`;
  reject(noncanonical);

  const blankAdapter = clone(fixture.receipt);
  blankAdapter.root.adapter_id = ' ';
  reject(blankAdapter);
});

test('close context itself rejects drift and unknown meaning', () => {
  const extra = clone(fixture.context);
  extra.future = true;
  assert.throws(() => parseScopedCloseContext(extra), ContractValidationError);

  const rootExtra = clone(fixture.context);
  rootExtra.root.future = true;
  assert.throws(() => parseScopedCloseContext(rootExtra), ContractValidationError);

  const missing = clone(fixture.context);
  delete missing.attachment_ref;
  assert.throws(() => parseScopedCloseContext(missing), ContractValidationError);
});

test('portable close parsing does not depend on the Node Buffer global', () => {
  const globalWithBuffer = globalThis as unknown as { Buffer?: unknown };
  const original = globalWithBuffer.Buffer;
  try {
    globalWithBuffer.Buffer = undefined;
    assert.deepEqual(parseScopedCloseReceipt(fixture.receipt, fixture.context), fixture.receipt);
  } finally {
    globalWithBuffer.Buffer = original;
  }
});

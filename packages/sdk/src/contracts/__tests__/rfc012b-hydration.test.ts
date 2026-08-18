import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import {
  catalogHydrationCommandBinding,
  catalogHydrationCommandsCoalesce,
  parseCatalogHydrationCommand,
  parseCatalogHydrationCommandBinding,
  parseCatalogSchedulingReceipt,
  type CatalogHydrationCommandExpectedContext,
  type CatalogSchedulingReceipt,
} from '../rfc012b-hydration.js';

interface HydrationFixture {
  fixture_contract_version: number;
  command: Record<string, unknown>;
  coalesced_command_binding: unknown;
  accepted_receipt: unknown;
  in_progress_receipt: unknown;
  retryable_receipt: unknown;
  retry_accepted_receipt: unknown;
  terminal_receipt: unknown;
  expected: {
    selected_base_is_presentation: boolean;
    coalesces: boolean;
    retry_attempt: number;
    raw_request_token_present: boolean;
    raw_native_identity_present: boolean;
    raw_locator_present: boolean;
    cancellation_contract_present: boolean;
  };
}

const rawFixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012b-catalog-hydration-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as HydrationFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function commandContext(command: Record<string, unknown>): CatalogHydrationCommandExpectedContext {
  return {
    identity: {
      request_key: command.request_key as never,
      command_id: command.command_id as never,
      coalescing_key: command.coalescing_key as never,
    },
    contract_selection: command.contract_selection,
    snapshot_id: command.snapshot_id,
    source: command.source,
    authorization: command.authorization,
    requested_scope: command.requested_scope,
    reason: 'selected_session',
  };
}

const expectedCommandContext = commandContext(rawFixture.command);

function parseFixtureCommand(value: unknown = rawFixture.command) {
  return parseCatalogHydrationCommand(value, expectedCommandContext);
}

function reject(value: unknown, parse: (input: unknown) => unknown): void {
  assert.throws(() => parse(value), ContractValidationError);
}

test('Rust hydration fixture preserves the portable command and exact coalescing contract', () => {
  assert.equal(rawFixture.fixture_contract_version, 1);
  const command = parseFixtureCommand();
  const commandBinding = catalogHydrationCommandBinding(command);
  const coalescedBinding = parseCatalogHydrationCommandBinding(rawFixture.coalesced_command_binding);

  assert.notEqual(commandBinding.request_key, coalescedBinding.request_key);
  assert.notEqual(commandBinding.command_id, coalescedBinding.command_id);
  assert.equal(commandBinding.coalescing_key, coalescedBinding.coalescing_key);
  assert.equal(catalogHydrationCommandsCoalesce(commandBinding, coalescedBinding), true);
  assert.equal(catalogHydrationCommandsCoalesce(coalescedBinding, commandBinding), true);
  assert.equal(rawFixture.expected.coalesces, true);
  assert.equal(
    command.authorization.handoff.selected_base_session_ref.external_ref.entity_key ===
      command.authorization.handoff.presentation_ref.external_ref.entity_key,
    rawFixture.expected.selected_base_is_presentation,
  );
});

test('portable command consumption binds identity, selection, snapshot, source, and locator authority', () => {
  const driftedIdentity = clone(rawFixture.command);
  driftedIdentity.command_id = (rawFixture.coalesced_command_binding as { command_id: string }).command_id;
  reject(driftedIdentity, parseFixtureCommand);

  const driftedSelection = clone(rawFixture.command) as {
    contract_selection: { typed_unknown: { max_payload_bytes: number } };
  };
  driftedSelection.contract_selection.typed_unknown.max_payload_bytes = 8_192;
  reject(driftedSelection, parseFixtureCommand);

  const driftedSnapshot = clone(rawFixture.command) as {
    snapshot_id: { complete_commit: number };
  };
  driftedSnapshot.snapshot_id.complete_commit += 1;
  reject(driftedSnapshot, parseFixtureCommand);

  const driftedSource = clone(rawFixture.command) as {
    source: { access_policy_digest: string; catalog_declaration_digest: string };
    authorization: { access_policy_digest: string; catalog_declaration_digest: string };
  };
  [driftedSource.source.access_policy_digest, driftedSource.source.catalog_declaration_digest] = [
    driftedSource.source.catalog_declaration_digest,
    driftedSource.source.access_policy_digest,
  ];
  driftedSource.authorization.access_policy_digest = driftedSource.source.access_policy_digest;
  driftedSource.authorization.catalog_declaration_digest = driftedSource.source.catalog_declaration_digest;
  reject(driftedSource, parseFixtureCommand);

  const driftedAuthorization = clone(rawFixture.command) as {
    authorization: { authorization_id: string; access_policy_digest: string };
  };
  driftedAuthorization.authorization.authorization_id = driftedAuthorization.authorization.access_policy_digest;
  reject(driftedAuthorization, parseFixtureCommand);

  const unsafeSnapshot = clone(rawFixture.command) as {
    snapshot_id: { readiness_epoch: number };
  };
  unsafeSnapshot.snapshot_id.readiness_epoch = Number.MAX_SAFE_INTEGER + 1;
  reject(unsafeSnapshot, parseFixtureCommand);

  const validScopeDrift = clone(rawFixture.command) as {
    requested_scope: { max_records_per_pass: number };
  };
  validScopeDrift.requested_scope.max_records_per_pass += 1;
  reject(validScopeDrift, parseFixtureCommand);

  const oversizedFamilyVersion = clone(rawFixture.command) as {
    contract_selection: { contract_versions: { fact_family_versions: Record<string, number> } };
    requested_scope: { fact_family_versions: Record<string, number> };
  };
  oversizedFamilyVersion.contract_selection.contract_versions.fact_family_versions['catalog.session'] = 0x1_0000_0000;
  oversizedFamilyVersion.requested_scope.fact_family_versions['catalog.session'] = 0x1_0000_0000;
  reject(oversizedFamilyVersion, (value) =>
    parseCatalogHydrationCommand(value, commandContext(oversizedFamilyVersion as never)),
  );

  const oversizedUnrequestedFamily = clone(rawFixture.command) as {
    contract_selection: { contract_versions: { fact_family_versions: Record<string, number> } };
  };
  oversizedUnrequestedFamily.contract_selection.contract_versions.fact_family_versions['catalog.message'] =
    0x1_0000_0000;
  reject(oversizedUnrequestedFamily, (value) =>
    parseCatalogHydrationCommand(value, commandContext(oversizedUnrequestedFamily as never)),
  );
});

test('portable hydration accepts a representative base member and rejects undisclosed or unbounded authority', () => {
  const representative = clone(rawFixture.command) as {
    authorization: {
      authorization_id: string;
      access_policy_digest: string;
      handoff: {
        presentation_ref: unknown;
        selected_base_session_ref: unknown;
        locator_claim_key: string;
        member_refs: unknown[];
        relation_keys: string[];
      };
    };
    request_key: string;
    command_id: string;
    coalescing_key: string;
  };
  representative.authorization.handoff.selected_base_session_ref =
    representative.authorization.handoff.presentation_ref;
  representative.authorization.authorization_id = representative.authorization.access_policy_digest;
  representative.request_key = representative.authorization.handoff.locator_claim_key;
  representative.command_id = representative.authorization.handoff.relation_keys[0]!;
  representative.coalescing_key = representative.authorization.authorization_id;
  assert.equal(
    parseCatalogHydrationCommand(representative, commandContext(representative)).authorization.handoff
      .selected_base_session_ref.external_ref.entity_key,
    parseCatalogHydrationCommand(representative, commandContext(representative)).authorization.handoff.presentation_ref
      .external_ref.entity_key,
  );

  const undisclosed = clone(representative);
  undisclosed.authorization.handoff.selected_base_session_ref = {
    kind: 'session',
    external_ref: {
      external_entity_reference_version: 1,
      entity_key: undisclosed.authorization.handoff.locator_claim_key,
    },
  };
  reject(undisclosed, (value) => parseCatalogHydrationCommand(value, commandContext(undisclosed)));

  const excessiveRelations = clone(rawFixture.command) as {
    authorization: { handoff: { relation_keys: string[] } };
  };
  excessiveRelations.authorization.handoff.relation_keys = Array.from(
    { length: 4_097 },
    () => excessiveRelations.authorization.handoff.relation_keys[0]!,
  );
  reject(excessiveRelations, (value) =>
    parseCatalogHydrationCommand(value, commandContext(excessiveRelations as never)),
  );

  const unbounded = clone(rawFixture.command) as {
    requested_scope: { max_records_per_pass: number };
  };
  unbounded.requested_scope.max_records_per_pass = 1_000_001;
  reject(unbounded, parseFixtureCommand);

  const rawLocator = clone(rawFixture.command) as {
    authorization: Record<string, unknown>;
  };
  rawLocator.authorization.raw_locator = '/private/forged/session.jsonl';
  reject(rawLocator, parseFixtureCommand);

  const additive = clone(rawFixture.command);
  additive.scheduler_private_state = true;
  reject(additive, parseFixtureCommand);

  for (const nested of ['source', 'snapshot_id'] as const) {
    const nestedUnknown = clone(rawFixture.command) as Record<string, Record<string, unknown>>;
    nestedUnknown[nested]!.future_authority = true;
    reject(nestedUnknown, parseFixtureCommand);
  }
  const handoffUnknown = clone(rawFixture.command) as {
    authorization: { handoff: Record<string, unknown> };
  };
  handoffUnknown.authorization.handoff.future_relation = true;
  reject(handoffUnknown, parseFixtureCommand);
  const entityUnknown = clone(rawFixture.command) as {
    authorization: { handoff: { selected_base_session_ref: Record<string, unknown> } };
  };
  entityUnknown.authorization.handoff.selected_base_session_ref.future_identity = true;
  reject(entityUnknown, parseFixtureCommand);

  const zeroGeneration = clone(rawFixture.command) as {
    authorization: { locator_source_generation: number };
  };
  zeroGeneration.authorization.locator_source_generation = 0;
  reject(zeroGeneration, (value) => parseCatalogHydrationCommand(value, commandContext(zeroGeneration as never)));

  const noncanonicalProvenance = clone(rawFixture.command) as {
    authorization: { locator_provenance: unknown[] };
  };
  assert.ok(noncanonicalProvenance.authorization.locator_provenance.length > 1);
  noncanonicalProvenance.authorization.locator_provenance.reverse();
  reject(noncanonicalProvenance, (value) =>
    parseCatalogHydrationCommand(value, commandContext(noncanonicalProvenance as never)),
  );

  const jsonParsedReserved = JSON.parse(JSON.stringify(rawFixture.command)) as {
    requested_scope: { fact_family_versions: Record<string, number> };
  };
  jsonParsedReserved.requested_scope.fact_family_versions = JSON.parse('{"__proto__":1}') as Record<string, number>;
  reject(jsonParsedReserved, (value) =>
    parseCatalogHydrationCommand(value, commandContext(jsonParsedReserved as never)),
  );
  assert.equal(({} as { polluted?: unknown }).polluted, undefined);
});

function parseReceipts(): {
  accepted: CatalogSchedulingReceipt;
  inProgress: CatalogSchedulingReceipt;
  retryable: CatalogSchedulingReceipt;
  retryAccepted: CatalogSchedulingReceipt;
  terminal: CatalogSchedulingReceipt;
} {
  const command = parseFixtureCommand();
  const commandBinding = catalogHydrationCommandBinding(command);
  const coalescedBinding = parseCatalogHydrationCommandBinding(rawFixture.coalesced_command_binding);
  const accepted = parseCatalogSchedulingReceipt(rawFixture.accepted_receipt, commandBinding, null, null);
  const inProgress = parseCatalogSchedulingReceipt(rawFixture.in_progress_receipt, coalescedBinding, null, {
    command: commandBinding,
    receipt: accepted,
  });
  const retryable = parseCatalogSchedulingReceipt(rawFixture.retryable_receipt, commandBinding, null, null);
  const retryAccepted = parseCatalogSchedulingReceipt(
    rawFixture.retry_accepted_receipt,
    commandBinding,
    retryable,
    null,
  );
  const terminal = parseCatalogSchedulingReceipt(rawFixture.terminal_receipt, commandBinding, null, null);
  return { accepted, inProgress, retryable, retryAccepted, terminal };
}

test('portable receipts preserve accepted, in-progress, retryable, and terminal lineage', () => {
  const { accepted, inProgress, retryable, retryAccepted, terminal } = parseReceipts();
  assert.equal(accepted.outcome.state, 'accepted');
  assert.equal(inProgress.outcome.state, 'in_progress');
  assert.equal(retryable.outcome.state, 'rejected');
  assert.equal(retryAccepted.attempt, rawFixture.expected.retry_attempt);
  assert.equal(retryAccepted.prior_receipt_id, retryable.receipt_id);
  assert.equal(terminal.outcome.state, 'rejected');
  if (terminal.outcome.state !== 'rejected') throw new Error('expected terminal rejection');
  assert.equal(terminal.outcome.failure.disposition, 'terminal');
  assert.equal(rawFixture.expected.cancellation_contract_present, false);
});

test('portable receipt consumption rejects forged prior, active, command, and failure lineage', () => {
  const command = parseFixtureCommand();
  const commandBinding = catalogHydrationCommandBinding(command);
  const coalescedBinding = parseCatalogHydrationCommandBinding(rawFixture.coalesced_command_binding);
  const { accepted, retryable, terminal } = parseReceipts();

  const forgedPrior = clone(rawFixture.retry_accepted_receipt) as { prior_receipt_id: string; command_id: string };
  forgedPrior.prior_receipt_id = forgedPrior.command_id;
  reject(forgedPrior, (value) => parseCatalogSchedulingReceipt(value, commandBinding, retryable, null));

  reject(rawFixture.retry_accepted_receipt, (value) =>
    parseCatalogSchedulingReceipt(value, commandBinding, accepted, null),
  );
  reject(rawFixture.retry_accepted_receipt, (value) =>
    parseCatalogSchedulingReceipt(value, commandBinding, terminal, null),
  );

  const forgedActive = clone(rawFixture.in_progress_receipt) as {
    outcome: { active_receipt_id: string };
    receipt_id: string;
  };
  forgedActive.outcome.active_receipt_id = forgedActive.receipt_id;
  const activeSchedule = { command: commandBinding, receipt: accepted };
  reject(forgedActive, (value) => parseCatalogSchedulingReceipt(value, coalescedBinding, null, activeSchedule));

  const forgedCommand = clone(rawFixture.in_progress_receipt) as { command_id: string };
  forgedCommand.command_id = accepted.command_id;
  reject(forgedCommand, (value) => parseCatalogSchedulingReceipt(value, coalescedBinding, null, activeSchedule));

  const staleAgainstActive = clone(rawFixture.in_progress_receipt) as { emitted_at_commit: number };
  staleAgainstActive.emitted_at_commit = accepted.snapshot_id.complete_commit;
  reject(staleAgainstActive, (value) => parseCatalogSchedulingReceipt(value, coalescedBinding, null, activeSchedule));

  reject(rawFixture.in_progress_receipt, (value) =>
    parseCatalogSchedulingReceipt(value, coalescedBinding, null, {
      command: coalescedBinding,
      receipt: accepted,
    }),
  );

  const malformedFailure = clone(rawFixture.terminal_receipt) as {
    outcome: { failure: { retry_after_millis: number } };
  };
  malformedFailure.outcome.failure.retry_after_millis = 1;
  reject(malformedFailure, (value) => parseCatalogSchedulingReceipt(value, commandBinding, null, null));

  for (const code of ['/private/native/error.txt', 'free form internal failure', 'UPPERCASE_FAILURE']) {
    const leakyFailure = clone(rawFixture.terminal_receipt) as {
      outcome: { failure: { code: string } };
    };
    leakyFailure.outcome.failure.code = code;
    reject(leakyFailure, (value) => parseCatalogSchedulingReceipt(value, commandBinding, null, null));
  }

  const attemptOverflow = clone(rawFixture.accepted_receipt) as { attempt: number };
  attemptOverflow.attempt = 0x1_0000_0000;
  reject(attemptOverflow, (value) => parseCatalogSchedulingReceipt(value, commandBinding, null, null));

  const overflowPrior = clone(retryable);
  overflowPrior.attempt = 0xffff_ffff;
  reject(rawFixture.retry_accepted_receipt, (value) =>
    parseCatalogSchedulingReceipt(value, commandBinding, overflowPrior, null),
  );

  const nestedSnapshotUnknown = clone(rawFixture.accepted_receipt) as {
    snapshot_id: Record<string, unknown>;
  };
  nestedSnapshotUnknown.snapshot_id.future_snapshot_authority = true;
  reject(nestedSnapshotUnknown, (value) => parseCatalogSchedulingReceipt(value, commandBinding, null, null));

  const additive = clone(rawFixture.accepted_receipt) as Record<string, unknown>;
  additive.scheduler_private_state = 'forged';
  reject(additive, (value) => parseCatalogSchedulingReceipt(value, commandBinding, null, null));
});

test('Rust hydration fixture and Debug projection contain no raw request, identity, or locator input', () => {
  const encoded = JSON.stringify(rawFixture);
  assert.equal(rawFixture.expected.raw_request_token_present, false);
  assert.equal(rawFixture.expected.raw_native_identity_present, false);
  assert.equal(rawFixture.expected.raw_locator_present, false);
  assert.equal(encoded.includes('raw-request-token-must-not-leak'), false);
  assert.equal(encoded.includes('raw-native-session-id-must-not-leak'), false);
  assert.equal(encoded.includes('/private/raw/session-locator-must-not-leak.jsonl'), false);
});

test('portable hydration validation does not depend on the Node Buffer global', () => {
  const globals = globalThis as unknown as { Buffer?: unknown };
  const originalBuffer = globals.Buffer;
  globals.Buffer = undefined;
  try {
    assert.equal(parseFixtureCommand().reason, 'selected_session');
  } finally {
    globals.Buffer = originalBuffer;
  }
});

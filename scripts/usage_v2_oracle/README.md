# Usage-v2 oracle

This directory owns the independent RFC 012C response-usage oracle. It does
not import the Spaghetti adapter, SDK, SQLite schema, or Rust output. Its input
is a bounded sanitized fixture containing source-object generations, framed
cursor ranges, and native Claude transcript records.

The oracle deliberately differs from `runtime_observation_census` in one
important respect: every token bucket is independently qualified. An omitted
bucket becomes `unknown/missing`; it never becomes numeric zero. The primary
Claude response key is a non-empty `message.id`. When that field is absent,
the oracle reproduces the object/generation-scoped framed cursor fallback
introduced in decoder contract 17 and retained unchanged by contract 18.
`requestId` remains metadata.

Run the fixture contract and checked-in report comparison with:

```sh
python3 scripts/usage_v2_oracle/test_oracle.py
```

Regenerate a candidate report for review with:

```sh
python3 scripts/usage_v2_oracle/oracle.py \
  agent-support/claude-code/candidate-2026-08-21/fixtures/usage-v2/response-revisions.json
```

The checked-in report is evidence, not a public runtime query. Promotion still
requires private corpus-scale parity, affiliation regrouping, readiness, and
the versioned query cutover described by RFC 012C.

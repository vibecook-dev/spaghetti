# Agent support contracts

This directory is the machine-readable release boundary for RFC 012A. An
adapter implementation is not evidence that a native artifact is supported.
Support is selected only from a **promoted** support-release entry whose
referenced contracts and evidence pass `scripts/agent_support/validate.py`.

Each candidate directory contains five independently versioned documents:

- `ads.json`: observed native data-surface and identity claims;
- `source-declarations.json`: bounded common-driver compositions;
- `scope-programs.json`: database-free, session-scoped relation programs;
- `evidence.json`: claim-addressable sanitized evidence;
- `conformance.json`: release gates and their current results; and
- `support-release.json`: the non-circular ledger entry that binds the other
  documents by SHA-256 digest.

Committed fixtures must be synthetic or produced by the deterministic
sanitizer. Raw captures, native paths, account identifiers, prompts, titles,
questions, payload text, and secrets are prohibited. The validator scans every
fixture even when its evidence claim is still open.

Candidate entries are intentionally non-selectable. Promotion requires a
separate reviewed change that resolves every blocker, supplies passing
conformance and performance reports, approves the sanitizer review, and pins
an exact or fixture-backed artifact range.

Run the gate with:

```sh
python3 scripts/agent_support/validate.py
python3 -m unittest scripts.agent_support.test_contracts
```

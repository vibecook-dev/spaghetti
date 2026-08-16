# Runtime observation census

This read-only RFC 012 experiment scans the JSONL objects selected by Claude's
current parent/subagent transcript declarations directly. It does not import
the Spaghetti adapter or SDK. It measures:

- response-level usage repetition and revision behavior;
- the difference between the current per-row delta interpretation and the
  last snapshot for each `(source object, message.id)` response;
- model/effort evidence coverage without emitting values;
- root/descendant transcript topology; and
- transcript evidence for plans, tasks, and `AskUserQuestion` lifecycles.

Run focused tests:

```sh
python3 scripts/runtime_observation_census/test_census.py
```

Run against the live Claude projects root:

```sh
python3 scripts/runtime_observation_census/census.py \
  --out /private/tmp/spaghetti-runtime-observation-census.json
```

The output is privacy-reduced: it contains aggregate counts, token totals, a
digest of source-file metadata, and timings. It never emits native paths,
identifiers, prompts, answers, model values, or raw payloads.

This is a static semantic census. It does not validate filesystem watcher
latency, attach-before-create behavior, bootstrap barriers, reset delivery, or
backpressure. Those belong to the scoped-observer conformance harness defined
by RFC 012.

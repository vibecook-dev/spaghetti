# Diagnostic aggregation census

This read-only experiment measures how many permanent diagnostic occurrences
can be represented by exact aggregate rows keyed by source object, generation,
and diagnostic signature.

```bash
python3 scripts/diagnostic_census/census.py \
  --database "$HOME/Library/Application Support/@vibecook/spaghetti-playground/Cache/spaghetti-rs.db"
```

The package alias is
`pnpm experiment:diagnostic-census --database <database-path>`.

The report does not emit error messages or native payloads. It records counts,
allocation, and hashed signatures. Aggregation remains a hypothesis until a
production implementation proves identical counts, first/last provenance,
bounded examples, replay behavior, and final canonical digests.

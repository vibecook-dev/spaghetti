# Bounded catalog census

This experiment tests whether Spaghetti can enumerate display-ready projects
and sessions without fully ingesting transcript bodies.

It reads native data independently from production adapters, measures logical
bytes read, and can compare project/session identities with an existing
Spaghetti database opened in read-only mode. Its privacy-reduced JSON output
contains only aggregate counts and identity digests; it does not emit native
paths, IDs, prompts, or titles.

```bash
python3 scripts/catalog_census/census.py

python3 scripts/catalog_census/census.py \
  --oracle-db "$HOME/Library/Application Support/@vibecook/spaghetti-playground/Cache/spaghetti-rs.db"

python3 scripts/catalog_census/test_census.py
```

The package aliases are `pnpm experiment:catalog-census` and
`pnpm test:catalog-census`; pnpm arguments are forwarded directly, for example
`pnpm experiment:catalog-census --head-bytes 65536`.

Use `--head-bytes` to measure the metadata-coverage/I/O curve. Claude session
indexes and Grok summaries use the separate `--document-bytes` bound.

The script is an experiment, not a reusable production parser. Validated
mechanics should become generic Rust source drivers and catalog facts; the
native interpretation should remain in agent adapters.

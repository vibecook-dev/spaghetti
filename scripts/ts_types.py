#!/usr/bin/env python3
"""
Read type facts out of the SDK's TypeScript sources.

The validators in this directory check real `~/.claude` data against the
SDK's types. They used to encode those types as hand-maintained Python
sets — and the copies, not the types, were what drifted. A 2026-07 audit
had already added `status`, `procStart`, `peerProtocol`, `bridgeSessionId`
and four more to `ActiveSessionFile`, but the Python still listed only
`{kind, entrypoint, name}`, so the suite reported eight "EXTRA keys not in
ActiveSessionFile type" that were in fact modelled. Likewise
`SessionMessageType` had gained `ai-title`, `bridge-session` and `mode`
while the validator went on calling them unknown.

Reading the declarations at runtime means a validator can only ever be as
stale as the types it is checking, which is the entire point of it.

These are regex readers, not a TypeScript parser. They rely on the
prettier formatting the repo enforces (two-space interface members, one
union member per line) and will exit non-zero rather than silently return
an empty set if a declaration cannot be found — an empty set would make
every check vacuously pass.
"""

from __future__ import annotations

import re
import sys
from functools import lru_cache
from pathlib import Path

TYPES_DIR = Path(__file__).resolve().parent.parent / "packages" / "sdk" / "src" / "types"


@lru_cache(maxsize=None)
def source(rel: str) -> str:
    """Read a file under packages/sdk/src/types, e.g. 'claude/projects.ts'."""
    path = TYPES_DIR / rel
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        _die(f"cannot read {path}: {exc}")


def _die(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    print(
        "       the TypeScript types moved or were renamed; update scripts/ts_types.py",
        file=sys.stderr,
    )
    sys.exit(2)


def union_members(type_name: str, rel: str) -> set[str]:
    """
    String-literal members of `export type <type_name> = 'a' | 'b' | ...`.

    Template-literal members such as `mcp__${string}` are not returned;
    callers that accept them match by prefix themselves.
    """
    return set(re.findall(r"'([^']+)'", _type_body(type_name, rel)))


def _interface_body(interface_name: str, rel: str) -> str:
    match = re.search(
        rf"interface {re.escape(interface_name)}[^{{]*\{{(.*?)^\}}",
        source(rel),
        re.DOTALL | re.MULTILINE,
    )
    if not match:
        _die(f"could not find `interface {interface_name}` in {rel}")
    return match.group(1)


def interface_fields(interface_name: str, rel: str) -> set[str]:
    """
    Property names declared directly on an interface, optional or not.

    Inherited members are not followed — callers that need them should
    union the base interface explicitly, which keeps this readable
    without a real type checker.
    """
    return set(re.findall(r"^ {2}(\w+)\??:", _interface_body(interface_name, rel), re.MULTILINE))


def interface_optional_fields(interface_name: str, rel: str) -> set[str]:
    """
    Only the `name?:` properties.

    Lets a validator say "declared but not guaranteed present" instead of
    forcing every modelled key to appear in real data — the distinction that
    separates a field the product dropped from a field we failed to model.
    """
    return set(re.findall(r"^ {2}(\w+)\?:", _interface_body(interface_name, rel), re.MULTILINE))


def declared_fields(rel: str) -> set[str]:
    """
    Every property name declared by any interface in a file.

    Interface members sit at exactly two spaces of indentation, which
    separates them from nested object literals and union members.
    """
    return set(re.findall(r"^ {2}(\w+)\??:", source(rel), re.MULTILINE))


def _type_body(type_name: str, rel: str) -> str:
    """
    Everything between `export type <name> =` and the `;` that closes it.

    Scanned by brace depth rather than matched with a non-greedy regex:
    object-shaped unions contain semicolons inside each variant, so
    `=(.*?);` would stop at the first field instead of the declaration end.
    """
    src = source(rel)
    match = re.search(rf"export type {re.escape(type_name)} =", src)
    if not match:
        _die(f"could not find `export type {type_name}` in {rel}")
    depth = 0
    for i in range(match.end(), len(src)):
        char = src[i]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        elif char == ";" and depth == 0:
            return src[match.end() : i]
    _die(f"unterminated `export type {type_name}` in {rel}")


def discriminated_variants(type_name: str, discriminant: str, rel: str) -> dict[str, set[str]]:
    """
    Map each variant of a discriminated union to its non-discriminant
    fields, e.g. `MarketplaceSource` ->
    `{'github': {'repo'}, 'git': {'url'}, 'directory': {'path'}}`.
    """
    body = _type_body(type_name, rel)
    variants: dict[str, set[str]] = {}
    for block in body.split("| {"):
        fields = dict(re.findall(r"(\w+)\??: *'?([^;'\n]*)'?;", block))
        kind = fields.pop(discriminant, None)
        if kind:
            variants[kind] = set(fields)
    return variants


def discriminant_values(field: str, rel: str) -> set[str]:
    """
    Every literal assigned to a discriminant field across a file, e.g.
    `subtype: 'compact_boundary'`. Used where variants are declared as
    separate interfaces rather than collected into one union.
    """
    return set(re.findall(rf"\b{re.escape(field)}: '([^']+)'", source(rel)))

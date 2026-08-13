/**
 * Chrome components — Header, Footer, HRule shared across all views
 */

import React from 'react';
import { Box, Text } from 'ink';
import type { IngestEngine } from '@vibecook/spaghetti-sdk';
import { useTerminalSize } from './hooks.js';

// ─── EngineBadge ───────────────────────────────────────────────────────

/**
 * Compact indicator of the ingest engine actually in use this session.
 * RFC 011 has one production owner. The compatibility prop remains typed as
 * `IngestEngine` while older renderer state is retired, but only Rust is shown.
 */
export function EngineBadge({ engine }: { engine: IngestEngine }): React.ReactElement {
  const isRust = engine === 'rs';
  const color = isRust ? 'green' : 'yellow';
  return (
    <Text>
      <Text dimColor>engine </Text>
      <Text color={color} bold>
        {'●'} {isRust ? 'RS' : 'LEGACY'}
      </Text>
      <Text> </Text>
    </Text>
  );
}

// ─── HRule ─────────────────────────────────────────────────────────────

export function HRule(): React.ReactElement {
  const { cols } = useTerminalSize();
  return <Text dimColor>{'─'.repeat(cols)}</Text>;
}

// ─── Header ────────────────────────────────────────────────────────────

export interface HeaderProps {
  breadcrumb: string;
  /** Optional second line (e.g. filter chips in messages view) */
  subtitle?: string;
}

export function Header({ breadcrumb, subtitle }: HeaderProps): React.ReactElement {
  return (
    <Box flexDirection="column">
      <Text> {breadcrumb}</Text>
      {subtitle ? <Text> {subtitle}</Text> : null}
      <HRule />
    </Box>
  );
}

// ─── Footer ────────────────────────────────────────────────────────────

export interface FooterProps {
  hints: string;
  /** Effective ingest engine — shown as a right-aligned badge. Omit to hide. */
  engine?: IngestEngine;
}

export function Footer({ hints, engine }: FooterProps): React.ReactElement {
  return (
    <Box flexDirection="column">
      <HRule />
      <Box>
        <Box flexGrow={1}>
          <Text dimColor> {hints}</Text>
        </Box>
        {engine ? <EngineBadge engine={engine} /> : null}
      </Box>
    </Box>
  );
}

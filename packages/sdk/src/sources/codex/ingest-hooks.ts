/**
 * Codex ingest hooks — token attribution for the shared IngestService.
 *
 * Codex does not put tokens on chat lines. After each model turn it emits
 * `token_count` events; we attribute `last_token_usage` to the preceding
 * assistant message, fall back to cumulative total, then tiktoken-estimate
 * when no official count was seen.
 *
 * Lives under `sources/codex/` so `data/ingest-service` stays product-free.
 */

import type { ExtractedMessage, IngestHooks, SessionTokenApi } from '../types.js';
import { parseCodexTokenCount, type CodexTokenUsage } from './token-usage.js';
import { estimateTokensFromMessageRows } from './estimate-tokens.js';

/** Stateful hooks factory — one instance per Codex IngestService. */
export function createCodexIngestHooks(): IngestHooks {
  /** Last assistant message written per session (for last_token_usage). */
  const lastAssistantBySession = new Map<string, { slug: string; msgIndex: number }>();
  /** Latest cumulative total_token_usage per session. */
  const lastTotalBySession = new Map<string, CodexTokenUsage>();
  /** Sessions that already received per-turn attribution. */
  const attributedTurnBySession = new Set<string>();

  return {
    onSessionStart(sessionId: string): void {
      lastAssistantBySession.delete(sessionId);
      lastTotalBySession.delete(sessionId);
      attributedTurnBySession.delete(sessionId);
    },

    onSkippedRecord(raw: unknown, ctx: { slug: string; sessionId: string }, api: SessionTokenApi): void {
      const parsed = parseCodexTokenCount(raw);
      if (!parsed) return;

      if (parsed.total) {
        lastTotalBySession.set(ctx.sessionId, parsed.total);
      }

      const usage = parsed.last ?? parsed.total;
      if (!usage) return;

      const target = lastAssistantBySession.get(ctx.sessionId);
      if (!target) {
        // token_count before any assistant turn — keep total for session-complete.
        return;
      }

      api.updateMessageTokens(ctx.sessionId, target.msgIndex, {
        inputTokens: usage.inputTokens,
        outputTokens: usage.outputTokens,
        cacheCreationTokens: usage.cacheCreationTokens,
        cacheReadTokens: usage.cacheReadTokens,
      });
      attributedTurnBySession.add(ctx.sessionId);

      // Clear pointer when we used total without last (avoid re-applying cum total).
      if (!parsed.last && parsed.total) {
        lastAssistantBySession.delete(ctx.sessionId);
      }
    },

    onMessageWritten(extracted: ExtractedMessage, ctx: { slug: string; sessionId: string; msgIndex: number }): void {
      if (extracted.msgType === 'assistant') {
        lastAssistantBySession.set(ctx.sessionId, { slug: ctx.slug, msgIndex: ctx.msgIndex });
      }
    },

    onSessionComplete(sessionId: string, api: SessionTokenApi): void {
      // Prefer official token_count (per-turn or cumulative fallback).
      if (!attributedTurnBySession.has(sessionId)) {
        const total = lastTotalBySession.get(sessionId);
        const target = lastAssistantBySession.get(sessionId);
        if (total && target) {
          api.updateMessageTokens(sessionId, target.msgIndex, {
            inputTokens: total.inputTokens,
            outputTokens: total.outputTokens,
            cacheCreationTokens: total.cacheCreationTokens,
            cacheReadTokens: total.cacheReadTokens,
          });
          attributedTurnBySession.add(sessionId);
          api.setSessionTokensEstimated(sessionId, false);
        }
      }

      if (!attributedTurnBySession.has(sessionId)) {
        // No official count — tiktoken-estimate stored text, but only for a
        // session where a turn actually finished.
        //
        // An assistant message is the marker: it is the model's reply, so it
        // proves both that the model ran and that the turn completed. A tool
        // call alone does not — the turn is still in flight, and the official
        // count arrives with the reply.
        //
        // Without this guard the estimator reported usage for work that never
        // happened: an aborted session with only a developer preamble, a
        // mid-turn tail whose reply had not been generated, and a question the
        // model never answered (RFC 008 Phase 3A, fixtures 05, 06, 10). Those
        // are not approximations of real usage — there is no usage to
        // approximate.
        const rows = api.listSessionMessageTexts(sessionId);
        const turnCompleted = rows.some((r) => r.msgType === 'assistant');
        if (rows.length === 0 || !turnCompleted) {
          api.setSessionTokensEstimated(sessionId, false);
        } else {
          const estimates = estimateTokensFromMessageRows(
            rows.map((r) => ({
              msg_index: r.msgIndex,
              msg_type: r.msgType,
              text_content: r.text,
            })),
          );
          if (estimates.length === 0) {
            api.setSessionTokensEstimated(sessionId, false);
          } else {
            for (const e of estimates) {
              api.updateMessageTokens(sessionId, e.msgIndex, {
                inputTokens: e.inputTokens,
                outputTokens: e.outputTokens,
                cacheCreationTokens: e.cacheCreationTokens,
                cacheReadTokens: e.cacheReadTokens,
              });
            }
            api.setSessionTokensEstimated(sessionId, true);
          }
        }
      } else {
        api.setSessionTokensEstimated(sessionId, false);
      }

      lastAssistantBySession.delete(sessionId);
      lastTotalBySession.delete(sessionId);
      attributedTurnBySession.delete(sessionId);
    },
  };
}

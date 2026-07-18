/**
 * Re-export display adapters from the SDK.
 *
 * Codex stores RolloutLine JSON and Grok stores chat_history in `messages.data`;
 * the TUI/CLI renderers expect Anthropic-style envelopes. The shared adapter
 * lives in `@vibecook/spaghetti-sdk` so the playground timeline uses the same path.
 */

export { adaptMessageForDisplay, adaptMessagesForDisplay } from '@vibecook/spaghetti-sdk';

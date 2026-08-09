//! Tiktoken token *estimate* for Codex sessions that carry no `token_count`.
//!
//! Port of `packages/sdk/src/sources/codex/estimate-tokens.ts`. Deliberately
//! not billing-accurate: it counts only stored prose, ignores system/tool/
//! image/cache overhead, and uses `o200k_base` (the GPT-4o family encoding).
//!
//! Callers must mark the session estimated so the UI can label it, and must
//! never let an estimate reach a session that has official usage.
//!
//! RFC 008 Phase 3A chose this policy — session-level fallback, narrowed to
//! sessions where a turn actually completed. See
//! `docs/rfcs/008-phase-3a-attribution.md`.

use once_cell::sync::Lazy;
use tiktoken_rs::CoreBPE;

/// Loading the encoding is non-trivial, and a large ingest hits it once per
/// un-attributed session. Build it once.
static ENCODING: Lazy<Option<CoreBPE>> = Lazy::new(|| tiktoken_rs::o200k_base().ok());

/// BPE token count for a string. Empty text is zero.
///
/// Falls back to a coarse `chars / 4` heuristic if the encoder is unavailable
/// or fails, matching the TS side — an ingest must not abort over an estimate
/// it was never going to be exact about.
pub fn count_text_tokens(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    match ENCODING.as_ref() {
        Some(bpe) => u32::try_from(bpe.encode_ordinary(text).len()).unwrap_or(u32::MAX),
        None => u32::try_from(text.chars().count().div_ceil(4)).unwrap_or(u32::MAX),
    }
}

/// Per-message estimate: `(input, output)`.
///
/// `user` and `developer` text counts as input, `assistant` text as output,
/// and every other row type contributes nothing. Note this differs from how
/// *official* counts are attributed — those put the whole prompt's input on
/// the assistant. The two shapes are only ever used for different sessions,
/// never mixed within one, which is what makes the difference tolerable.
pub fn estimate_for(msg_type: &str, text: &str) -> Option<(u32, u32)> {
    let n = count_text_tokens(text);
    if n == 0 {
        return None;
    }
    match msg_type {
        "user" | "developer" => Some((n, 0)),
        "assistant" => Some((0, n)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_zero_and_not_emitted() {
        assert_eq!(count_text_tokens(""), 0);
        assert_eq!(estimate_for("user", ""), None);
    }

    #[test]
    fn user_and_developer_count_as_input_assistant_as_output() {
        assert_eq!(estimate_for("user", "hello world"), Some((2, 0)));
        assert_eq!(estimate_for("developer", "hello world"), Some((2, 0)));
        assert_eq!(estimate_for("assistant", "hello world"), Some((0, 2)));
    }

    #[test]
    fn other_row_types_contribute_nothing() {
        // Tool calls and their output are not prose the model was billed for
        // in any way this estimate can model.
        assert!(estimate_for("tool_use", "shell_command").is_none());
        assert!(estimate_for("tool_result", " M pnpm-lock.yaml").is_none());
    }

    #[test]
    fn matches_the_ts_encoding_on_a_known_string() {
        // o200k_base. If this drifts, the cross-engine diff will disagree on
        // every estimated session, so pin it here where the failure is legible.
        assert_eq!(
            count_text_tokens("What does this function return on an empty slice?"),
            10
        );
        assert_eq!(count_text_tokens("Thanks."), 2);
    }
}

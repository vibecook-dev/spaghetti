//! Grok sidecar enrichment — timestamps (events.jsonl) + session tokens (signals.json).
//!
//! Behaviour-aligned with `packages/sdk/src/sources/grok/sidecars.ts`.
//!
//! # Timestamp join (turn-scoped)
//!
//! 1. `turn_started.conversation_message_count` = absolute chat_history index
//!    of that turn's primary user message. Compaction resets the counter while
//!    retaining old events, so only the latest valid counter epoch is joined.
//! 2. Turn ranges: `[count_i, count_{i+1})`.
//! 3. Within `[turn_started.ts, next turn_started.ts)`, pair `loop_started` /
//!    `first_token` with assistant cycles; multiple `reasoning` rows may
//!    share the current loop before the assistant advances it.
//! 4. Pre-turn lines get `fallback_created` (summary.created_at).

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

/// Absolute chat_history line index → ISO timestamp.
pub type TimestampMap = HashMap<u32, String>;

#[derive(Debug, Clone, Default)]
pub struct GrokSignals {
    pub context_tokens_used: u64,
}

#[derive(Debug, Clone)]
struct EventLine {
    ty: String,
    ts: String,
    conversation_message_count: Option<u32>,
}

fn parse_events(text: &str) -> Vec<EventLine> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let ty = v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let ts = v.get("ts").and_then(Value::as_str).unwrap_or("").to_owned();
        if ty.is_empty() || ts.is_empty() {
            continue;
        }
        out.push(EventLine {
            ty,
            ts,
            conversation_message_count: v
                .get("conversation_message_count")
                .and_then(Value::as_u64)
                .map(|n| n as u32),
        });
    }
    out
}

/// Collect `type` for each non-empty chat_history line.
pub fn collect_line_types(chat_text: &str) -> Vec<String> {
    let mut types = Vec::new();
    for line in chat_text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => types.push(
                v.get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
            ),
            Err(_) => types.push("unknown".into()),
        }
    }
    types
}

fn clamp_index(v: i64, lo: usize, hi: usize) -> usize {
    if hi <= lo {
        return lo;
    }
    if v < lo as i64 {
        return lo;
    }
    if v as usize > hi {
        return hi;
    }
    v as usize
}

/// Build absolute-line-index → timestamp map (mirrors TS `buildTimestampMap`).
pub fn build_timestamp_map(
    line_types: &[String],
    events_text: &str,
    fallback_created: Option<&str>,
) -> TimestampMap {
    let n = line_types.len();
    let mut map: TimestampMap = HashMap::new();
    if n == 0 {
        return map;
    }

    let events = parse_events(events_text);
    let candidates: Vec<&EventLine> = events
        .iter()
        .filter(|event| event.ty == "turn_started" && event.conversation_message_count.is_some())
        .collect();
    let mut epochs: Vec<Vec<&EventLine>> = Vec::new();
    for turn in candidates {
        let current = turn.conversation_message_count.unwrap_or(0);
        let starts_new = epochs
            .last()
            .and_then(|epoch| epoch.last())
            .and_then(|previous| previous.conversation_message_count)
            .is_some_and(|previous| current < previous);
        if epochs.is_empty() || starts_new {
            epochs.push(vec![turn]);
        } else if let Some(epoch) = epochs.last_mut() {
            epoch.push(turn);
        }
    }
    let turns: Vec<&EventLine> = epochs
        .iter()
        .rev()
        .find_map(|epoch| {
            let valid: Vec<&EventLine> = epoch
                .iter()
                .copied()
                .filter(|turn| {
                    let index = turn.conversation_message_count.unwrap_or(u32::MAX) as usize;
                    index < n && line_types[index] == "user"
                })
                .collect();
            (!valid.is_empty()).then_some(valid)
        })
        .unwrap_or_default();

    let first_turn_start = turns
        .first()
        .and_then(|t| t.conversation_message_count)
        .map(|c| clamp_index(c as i64, 0, n))
        .unwrap_or(n);

    if let Some(fb) = fallback_created {
        for (i, t) in line_types.iter().enumerate().take(first_turn_start) {
            if t == "system" || t == "user" {
                map.insert(i as u32, fb.to_owned());
            }
        }
    }

    if turns.is_empty() {
        if let Some(fb) = fallback_created {
            for (i, t) in line_types.iter().enumerate().skip(first_turn_start) {
                if t == "system" || t == "user" {
                    map.insert(i as u32, fb.to_owned());
                }
            }
        }
        return map;
    }

    for (ti, turn) in turns.iter().enumerate() {
        let start = clamp_index(turn.conversation_message_count.unwrap_or(0) as i64, 0, n);
        let end = if ti + 1 < turns.len() {
            clamp_index(
                turns[ti + 1].conversation_message_count.unwrap_or(n as u32) as i64,
                start,
                n,
            )
        } else {
            n
        };

        let window_start = turn.ts.as_str();
        let window_end: &str = if ti + 1 < turns.len() {
            turns[ti + 1].ts.as_str()
        } else {
            "\u{ffff}"
        };

        let loops: Vec<&str> = events
            .iter()
            .filter(|e| {
                e.ty == "loop_started"
                    && e.ts.as_str() >= window_start
                    && e.ts.as_str() < window_end
            })
            .map(|e| e.ts.as_str())
            .collect();
        let first_tokens: Vec<&str> = events
            .iter()
            .filter(|e| {
                e.ty == "first_token" && e.ts.as_str() >= window_start && e.ts.as_str() < window_end
            })
            .map(|e| e.ts.as_str())
            .collect();

        let mut loop_i: usize = 0;
        let mut last_agent_timestamp = turn.ts.clone();
        for (i, line_ty) in line_types.iter().enumerate().take(end).skip(start) {
            let t = line_ty.as_str();
            let idx = i as u32;
            match t {
                "user" | "system" => {
                    map.insert(idx, turn.ts.clone());
                }
                "reasoning" => {
                    if loop_i < loops.len() {
                        map.insert(idx, loops[loop_i].to_owned());
                    } else if !loops.is_empty() {
                        map.insert(idx, loops[loops.len() - 1].to_owned());
                    } else {
                        map.insert(idx, turn.ts.clone());
                    }
                }
                "assistant" => {
                    let assistant_timestamp = if loop_i < first_tokens.len() {
                        first_tokens[loop_i].to_owned()
                    } else if loop_i < loops.len() {
                        loops[loop_i].to_owned()
                    } else {
                        turn.ts.clone()
                    };
                    map.insert(idx, assistant_timestamp.clone());
                    last_agent_timestamp = assistant_timestamp;
                    loop_i = loop_i.saturating_add(1);
                }
                "tool_result" => {
                    map.insert(idx, last_agent_timestamp.clone());
                }
                "backend_tool_call" => {
                    let timestamp = loops
                        .get(loop_i)
                        .map(|timestamp| (*timestamp).to_owned())
                        .unwrap_or_else(|| last_agent_timestamp.clone());
                    map.insert(idx, timestamp);
                }
                _ => {}
            }
        }
    }

    map
}

/// Mirror of TS `grokRecordWeight`.
///
/// The weight is a *length*, so key ordering is irrelevant — reordering an
/// object's keys cannot change how many characters it serializes to, which is
/// why `serde_json`'s sorted `Map` is fine here. Length is counted in UTF-16
/// code units because the TS side measures `String.prototype.length`; bytes or
/// `chars()` would disagree on any astral-plane character.
fn record_weight(record: &Value) -> u64 {
    let serialized = match record.get("type").and_then(Value::as_str).unwrap_or("") {
        "system" | "user" => stringify_or_raw(record.get("content")),
        "assistant" => {
            // TS builds the array `[content, tool_calls]`, and a missing member
            // stringifies as `null` rather than vanishing.
            let content = record.get("content").cloned().unwrap_or(Value::Null);
            let tool_calls = record.get("tool_calls").cloned().unwrap_or(Value::Null);
            serde_json::to_string(&Value::Array(vec![content, tool_calls])).unwrap_or_default()
        }
        // encrypted_content is ciphertext, not a meaningful token proxy.
        "reasoning" => stringify_or_raw(record.get("summary")),
        "tool_result" | "backend_tool_call" => serde_json::to_string(record).unwrap_or_default(),
        _ => return 0,
    };
    (serialized.encode_utf16().count() as u64).max(1)
}

/// TS: `typeof value === 'string' ? value : JSON.stringify(value ?? '')`.
fn stringify_or_raw(value: Option<&Value>) -> String {
    match value {
        // A raw string is measured unquoted and unescaped.
        Some(Value::String(s)) => s.clone(),
        // `value ?? ''` folds both null and undefined to the empty string, which
        // JSON.stringify renders as the two-character `""`.
        None | Some(Value::Null) => "\"\"".to_owned(),
        Some(v) => serde_json::to_string(v).unwrap_or_default(),
    }
}

/// Largest-remainder allocation keeps the estimated session total exact.
///
/// Port of TS `distributeGrokSessionTokens`. The arithmetic runs in f64 in the
/// same order as the TS expression so both engines land on the same floor and
/// the same remainder ranking.
pub fn distribute_session_tokens(
    chat_text: &str,
    total_tokens: u64,
    eligible: &std::collections::HashSet<u32>,
) -> HashMap<u32, u64> {
    struct Row {
        index: u32,
        weight: u64,
        fraction: f64,
        tokens: u64,
    }

    let mut weighted: Vec<Row> = Vec::new();
    let mut absolute_index: u32 = 0;
    for line in chat_text.split('\n') {
        // Blank lines are skipped WITHOUT advancing the index; unparseable ones
        // still advance it. Both match TS, and the index is what the message
        // rows are keyed on, so drifting here misattributes every later row.
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<Value>(line) {
            let weight = if eligible.contains(&absolute_index) {
                record_weight(&record)
            } else {
                0
            };
            if weight > 0 {
                weighted.push(Row {
                    index: absolute_index,
                    weight,
                    fraction: 0.0,
                    tokens: 0,
                });
            }
        }
        absolute_index = absolute_index.saturating_add(1);
    }

    let weight_total: u64 = weighted.iter().map(|row| row.weight).sum();
    if total_tokens == 0 || weight_total == 0 {
        return HashMap::new();
    }

    let mut assigned: u64 = 0;
    for row in weighted.iter_mut() {
        let exact = (total_tokens as f64 * row.weight as f64) / weight_total as f64;
        let floored = exact.floor();
        row.tokens = floored as u64;
        row.fraction = exact - floored;
        assigned = assigned.saturating_add(row.tokens);
    }

    weighted.sort_by(|a, b| {
        b.fraction
            .partial_cmp(&a.fraction)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.index.cmp(&b.index))
    });
    let leftover = total_tokens.saturating_sub(assigned);
    if !weighted.is_empty() {
        for i in 0..leftover {
            let slot = (i % weighted.len() as u64) as usize;
            weighted[slot].tokens = weighted[slot].tokens.saturating_add(1);
        }
    }

    weighted
        .into_iter()
        .filter(|row| row.tokens > 0)
        .map(|row| (row.index, row.tokens))
        .collect()
}

pub fn parse_signals(text: &str) -> Option<GrokSignals> {
    let v: Value = serde_json::from_str(text).ok()?;
    let used = v
        .get("contextTokensUsed")
        .or_else(|| v.get("context_tokens_used"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if used == 0 {
        return None;
    }
    Some(GrokSignals {
        context_tokens_used: used,
    })
}

/// Timestamp map, signals, line types, and the per-line token allocation.
pub struct Sidecars {
    pub ts_map: TimestampMap,
    pub signals: Option<GrokSignals>,
    pub line_types: Vec<String>,
    /// Absolute chat_history line index → estimated input tokens.
    pub tokens: HashMap<u32, u64>,
}

/// Load timestamp map + signals from sibling files next to chat_history.
pub fn load_sidecars(chat_history: &Path, fallback_created: Option<&str>) -> Sidecars {
    let session_dir = match chat_history.parent() {
        Some(p) => p,
        None => {
            return Sidecars {
                ts_map: HashMap::new(),
                signals: None,
                line_types: Vec::new(),
                tokens: HashMap::new(),
            }
        }
    };

    let chat_text = std::fs::read_to_string(chat_history).unwrap_or_default();
    let line_types = collect_line_types(&chat_text);

    let events_text = std::fs::read_to_string(session_dir.join("events.jsonl")).unwrap_or_default();
    let ts_map = build_timestamp_map(&line_types, &events_text, fallback_created);

    let signals = std::fs::read_to_string(session_dir.join("signals.json"))
        .ok()
        .and_then(|t| parse_signals(&t));

    // Same eligibility set as TS: only lines that earned a timestamp.
    let tokens = match signals.as_ref() {
        Some(sig) if sig.context_tokens_used > 0 => {
            let eligible: std::collections::HashSet<u32> = ts_map.keys().copied().collect();
            distribute_session_tokens(&chat_text, sig.context_tokens_used, &eligible)
        }
        _ => HashMap::new(),
    };

    Sidecars {
        ts_map,
        signals,
        line_types,
        tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const ALLOC_LINES: &str = concat!(
        r#"{"type":"user","content":"short"}"#,
        "\n",
        r#"{"type":"assistant","content":"a much longer response"}"#,
        "\n",
        r#"{"type":"reasoning","summary":"think","encrypted_content":"ignored-ciphertext"}"#,
    );

    /// Exact values, cross-checked against the TS implementation. Weights are
    /// 5 / 31 / 5 — the assistant serializes as `["a much longer response",null]`
    /// and `encrypted_content` is excluded — so 101 splits 12 / 76 / 12 with the
    /// single leftover going to the largest remainder.
    #[test]
    fn distributes_session_tokens_like_the_ts_writer() {
        let alloc =
            distribute_session_tokens(ALLOC_LINES, 101, &HashSet::from([0_u32, 1_u32, 2_u32]));
        assert_eq!(alloc.get(&0), Some(&12));
        assert_eq!(alloc.get(&1), Some(&77));
        assert_eq!(alloc.get(&2), Some(&12));
        assert_eq!(alloc.values().sum::<u64>(), 101, "total must stay exact");
    }

    /// Ineligible lines (no timestamp) drop out of the weighting entirely rather
    /// than taking a zero share, so the total redistributes across the rest.
    #[test]
    fn ineligible_lines_are_excluded_from_the_weighting() {
        let alloc = distribute_session_tokens(ALLOC_LINES, 101, &HashSet::from([0_u32, 1_u32]));
        assert_eq!(alloc.get(&0), Some(&14));
        assert_eq!(alloc.get(&1), Some(&87));
        assert_eq!(alloc.get(&2), None);
        assert_eq!(alloc.values().sum::<u64>(), 101);
    }

    /// Weights count UTF-16 code units, matching JS `String.length`. An emoji is
    /// 2 units but 4 bytes, so a byte-length port would weight these unevenly
    /// and hand out 7/3 instead of 5/5.
    #[test]
    fn weights_count_utf16_units_not_bytes() {
        let lines = concat!(
            r#"{"type":"user","content":"😀"}"#,
            "\n",
            r#"{"type":"user","content":"ab"}"#,
        );
        let alloc = distribute_session_tokens(lines, 10, &HashSet::from([0_u32, 1_u32]));
        assert_eq!(alloc.get(&0), Some(&5));
        assert_eq!(alloc.get(&1), Some(&5));
    }

    /// Blank lines do not advance the index but unparseable ones do — the index
    /// is what message rows are keyed on, so drift here misattributes tokens.
    #[test]
    fn blank_lines_do_not_advance_the_index_but_bad_json_does() {
        let lines = concat!(
            "\n",
            r#"{"type":"user","content":"aa"}"#,
            "\n",
            "{ not json",
            "\n",
            r#"{"type":"user","content":"bb"}"#,
        );
        // Index 0 = first real line, 1 = the bad JSON, 2 = the last line.
        let alloc = distribute_session_tokens(lines, 8, &HashSet::from([0_u32, 2_u32]));
        assert_eq!(alloc.get(&0), Some(&4));
        assert_eq!(alloc.get(&2), Some(&4));
    }

    #[test]
    fn turn_scoped_join_exact_user_and_loop_pairing() {
        // Pre-turn: system + bootstrap users at 0..2; turn0 user at index 2.
        let types = vec![
            "system".into(),
            "user".into(), // bootstrap
            "user".into(), // turn0 primary (conversation_message_count=2)
            "reasoning".into(),
            "reasoning".into(), // same loop
            "assistant".into(),
            "tool_result".into(),
            "reasoning".into(),
            "assistant".into(),
            // turn1
            "user".into(), // conversation_message_count=9
            "reasoning".into(),
            "assistant".into(),
        ];
        let events = r#"
{"ts":"2026-04-01T10:00:00.000Z","type":"turn_started","turn_number":0,"conversation_message_count":2}
{"ts":"2026-04-01T10:00:01.000Z","type":"loop_started","loop_index":0}
{"ts":"2026-04-01T10:00:02.000Z","type":"first_token"}
{"ts":"2026-04-01T10:00:10.000Z","type":"loop_started","loop_index":1}
{"ts":"2026-04-01T10:00:11.000Z","type":"first_token"}
{"ts":"2026-04-01T10:00:20.000Z","type":"turn_ended"}
{"ts":"2026-04-01T11:00:00.000Z","type":"turn_started","turn_number":1,"conversation_message_count":9}
{"ts":"2026-04-01T11:00:01.000Z","type":"loop_started","loop_index":0}
{"ts":"2026-04-01T11:00:02.000Z","type":"first_token"}
{"ts":"2026-04-01T11:00:10.000Z","type":"turn_ended"}
"#;
        let map = build_timestamp_map(&types, events, Some("2026-04-01T09:00:00.000Z"));

        // Pre-turn bootstrap
        assert_eq!(
            map.get(&0).map(String::as_str),
            Some("2026-04-01T09:00:00.000Z")
        );
        assert_eq!(
            map.get(&1).map(String::as_str),
            Some("2026-04-01T09:00:00.000Z")
        );
        // Turn 0 user
        assert_eq!(
            map.get(&2).map(String::as_str),
            Some("2026-04-01T10:00:00.000Z")
        );
        // Both reasonings in loop 0 share loop_started
        assert_eq!(
            map.get(&3).map(String::as_str),
            Some("2026-04-01T10:00:01.000Z")
        );
        assert_eq!(
            map.get(&4).map(String::as_str),
            Some("2026-04-01T10:00:01.000Z")
        );
        // Assistants get first_token and advance loop
        assert_eq!(
            map.get(&5).map(String::as_str),
            Some("2026-04-01T10:00:02.000Z")
        );
        assert_eq!(
            map.get(&6).map(String::as_str),
            Some("2026-04-01T10:00:02.000Z")
        ); // result shares call timestamp
        assert_eq!(
            map.get(&7).map(String::as_str),
            Some("2026-04-01T10:00:10.000Z")
        );
        assert_eq!(
            map.get(&8).map(String::as_str),
            Some("2026-04-01T10:00:11.000Z")
        );
        // Turn 1
        assert_eq!(
            map.get(&9).map(String::as_str),
            Some("2026-04-01T11:00:00.000Z")
        );
        assert_eq!(
            map.get(&10).map(String::as_str),
            Some("2026-04-01T11:00:01.000Z")
        );
        assert_eq!(
            map.get(&11).map(String::as_str),
            Some("2026-04-01T11:00:02.000Z")
        );
    }

    #[test]
    fn compaction_uses_latest_valid_counter_epoch() {
        let types = vec![
            "system".into(),
            "user".into(),
            "reasoning".into(),
            "assistant".into(),
            "user".into(),
            "assistant".into(),
        ];
        let events = r#"
{"ts":"2026-04-01T08:00:00.000Z","type":"turn_started","turn_number":20,"conversation_message_count":10}
{"ts":"2026-04-01T08:10:00.000Z","type":"turn_started","turn_number":21,"conversation_message_count":20}
{"ts":"2026-04-01T10:00:00.000Z","type":"turn_started","turn_number":0,"conversation_message_count":1}
{"ts":"2026-04-01T10:00:01.000Z","type":"loop_started"}
{"ts":"2026-04-01T10:00:02.000Z","type":"first_token"}
{"ts":"2026-04-01T11:00:00.000Z","type":"turn_started","turn_number":1,"conversation_message_count":4}
{"ts":"2026-04-01T11:00:01.000Z","type":"loop_started"}
{"ts":"2026-04-01T11:00:02.000Z","type":"first_token"}
"#;
        let map = build_timestamp_map(&types, events, Some("2026-04-01T09:00:00.000Z"));
        assert_eq!(
            map.get(&0).map(String::as_str),
            Some("2026-04-01T09:00:00.000Z")
        );
        assert_eq!(
            map.get(&1).map(String::as_str),
            Some("2026-04-01T10:00:00.000Z")
        );
        assert_eq!(
            map.get(&3).map(String::as_str),
            Some("2026-04-01T10:00:02.000Z")
        );
        assert_eq!(
            map.get(&4).map(String::as_str),
            Some("2026-04-01T11:00:00.000Z")
        );
        assert_eq!(
            map.get(&5).map(String::as_str),
            Some("2026-04-01T11:00:02.000Z")
        );
    }

    #[test]
    fn parses_signals_context_tokens() {
        let s =
            parse_signals(r#"{"contextTokensUsed":106352,"contextWindowTokens":500000}"#).unwrap();
        assert_eq!(s.context_tokens_used, 106352);
    }
}

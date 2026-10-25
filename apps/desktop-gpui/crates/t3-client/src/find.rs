//! Client side of `orchestration.searchThread`, with server-owned match ordering.

use std::ops::Range;

use serde::Deserialize;
use serde_json::{Value, json};

pub const SEARCH: &str = "orchestration.searchThread";
pub const SEARCH_STREAM: &str = "orchestration.searchThreadStream";
pub const MAX_QUERY_LENGTH: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Match {
    pub entry_id: String,
    pub occurrence: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub snapshot_sequence: u64,
    pub total_matches: usize,
    pub active_index: usize,
    #[serde(rename = "match")]
    pub selected: Option<Match>,
    pub complete: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Request {
    generation: u64,
    pub query: String,
    start: Option<Match>,
    offset: i32,
}

impl Request {
    pub fn input(&self, thread_id: &str) -> Value {
        let mut input =
            json!({ "threadId": thread_id, "query": self.query, "offset": self.offset });
        if let Some(start) = &self.start {
            input["start"] = json!({ "entryId": start.entry_id, "occurrence": start.occurrence });
        }
        input
    }
}

#[derive(Debug, Default)]
pub struct State {
    pub query: String,
    pub result: Option<SearchResult>,
    generation: u64,
}

impl State {
    pub fn set_query(&mut self, query: &str) -> bool {
        let query = query.trim();
        if query == self.query {
            return false;
        }
        self.query = query.to_owned();
        self.result = None;
        self.generation += 1;
        true
    }

    pub fn valid_query(&self) -> bool {
        !self.query.is_empty() && self.query.encode_utf16().count() <= MAX_QUERY_LENGTH
    }

    pub fn begin(&mut self, offset: i32) -> Option<Request> {
        if !self.valid_query() {
            return None;
        }
        self.generation += 1;
        Some(Request {
            generation: self.generation,
            query: self.query.clone(),
            start: self
                .result
                .as_ref()
                .and_then(|result| result.selected.clone()),
            offset,
        })
    }

    pub fn is_current(&self, request: &Request) -> bool {
        self.generation == request.generation && self.query == request.query
    }

    pub fn apply(&mut self, request: &Request, result: SearchResult) -> bool {
        if !self.is_current(request) {
            return false;
        }
        self.result = Some(result);
        true
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.result = None;
        self.generation += 1;
    }
}

/// The web's `findThreadSearchOccurrences`, mapped back to UTF-8 ranges for GPUI.
/// Case folding preserves UTF-16 width, as the server does, including final sigma.
pub fn ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let (text, boundaries) = fold(text);
    let (query, _) = fold(query);
    text.match_indices(&query)
        .filter_map(|(start, matched)| Some(boundaries[start]?..boundaries[start + matched.len()]?))
        .collect()
}

fn fold(text: &str) -> (String, Vec<Option<usize>>) {
    let mut folded = String::new();
    let mut boundaries = vec![Some(0)];
    for (start, character) in text.char_indices() {
        let lower = character
            .to_lowercase()
            .collect::<String>()
            .replace('ς', "σ");
        if lower.encode_utf16().count() == character.len_utf16() {
            folded.push_str(&lower);
        } else {
            folded.push(character);
        }
        let previous_end = boundaries.len() - 1;
        boundaries.resize(folded.len() + 1, None);
        boundaries[previous_end] = Some(start);
        boundaries[folded.len()] = Some(start + character.len_utf8());
    }
    (folded, boundaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(entry: &str, occurrence: usize) -> SearchResult {
        serde_json::from_value(
            json!({"snapshotSequence": 9, "totalMatches": 3, "activeIndex": 1,
            "match": {"entryId": entry, "runId": null, "occurrence": occurrence}}),
        )
        .unwrap()
    }

    #[test]
    fn query_edits_and_close_invalidate_pending_searches() {
        let mut state = State::default();
        state.set_query("  before ");
        let old = state.begin(0).unwrap();
        assert_eq!(old.input("thread")["query"], "before");
        state.set_query("after");
        assert!(!state.apply(&old, result("old", 0)));
        let current = state.begin(0).unwrap();
        assert!(state.apply(&current, result("new", 1)));
        state.clear();
        assert!(!state.apply(&current, result("new", 1)));
        assert!(state.result.is_none());
        assert!(state.begin(0).is_none());
    }

    #[test]
    fn navigation_anchors_to_identity_and_occurrence_instead_of_a_stale_index() {
        let mut state = State::default();
        state.set_query("needle");
        let request = state.begin(0).unwrap();
        state.apply(&request, result("message", 2));
        let next = state.begin(1).unwrap();
        assert_eq!(
            next.input("thread")["start"],
            json!({"entryId":"message","occurrence":2})
        );
        assert_eq!(next.input("thread")["offset"], 1);
        assert!(!state.apply(&request, result("late", 0)));
        assert_eq!(state.begin(-1).unwrap().input("thread")["offset"], -1);
    }

    #[test]
    fn queries_obey_the_existing_utf16_wire_limit() {
        let mut state = State::default();
        state.set_query(&"😀".repeat(100));
        assert!(state.valid_query());
        state.set_query(&"😀".repeat(101));
        assert!(state.begin(0).is_none());
        state.set_query(" \n ");
        assert!(state.begin(0).is_none());
    }

    #[test]
    fn progressive_counts_update_the_same_request_without_replacing_a_newer_search() {
        let mut state = State::default();
        state.set_query("needle");
        let request = state.begin(0).unwrap();
        let mut first = result("message", 0);
        first.complete = Some(false);
        first.total_matches = 1;
        assert!(state.apply(&request, first));
        let mut final_result = result("message", 0);
        final_result.complete = Some(true);
        assert!(state.apply(&request, final_result));
        assert_eq!(state.result.as_ref().unwrap().total_matches, 3);
        let newer = state.begin(1).unwrap();
        assert!(!state.apply(&request, result("stale", 0)));
        assert!(state.apply(&newer, result("message", 1)));
        assert_eq!(state.result.unwrap().selected.unwrap().occurrence, 1);
    }

    #[test]
    fn highlights_are_nonoverlapping_case_insensitive_utf8_ranges() {
        assert_eq!(ranges("banana BANANA", "ana"), [1..4, 8..11]);
        assert_eq!(ranges("K kelvin Σσς", "k"), [0..3, 4..5]);
        assert_eq!(ranges("Σσς", "σ"), [0..2, 2..4, 4..6]);
        assert!(ranges("İ", "i").is_empty());
        let emoji = ranges("😀 needle", "NEEDLE");
        assert_eq!(emoji.len(), 1);
        assert_eq!(emoji[0], 5..11);
        assert!(ranges("needle", "").is_empty());
    }
}

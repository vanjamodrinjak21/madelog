//! Appending to and verifying the hash chain.
//!
//! The chain is the whole point: an event's hash covers the previous hash, so
//! editing one row invalidates every row after it.

use crate::event::{hash_for, Event, EventKind, GENESIS_PREV_HASH};

/// Build the next event in a chain. Pure: no clock, no database.
pub fn next_event(
    prev: Option<&Event>,
    id: String,
    ts: String,
    kind: EventKind,
    commit_sha: Option<String>,
    payload: serde_json::Value,
) -> Event {
    let (seq, prev_hash) = match prev {
        Some(p) => (p.seq + 1, p.hash.clone()),
        None => (1, GENESIS_PREV_HASH.to_string()),
    };
    let hash = hash_for(&prev_hash, &ts, kind, &payload);
    Event {
        seq,
        id,
        ts,
        kind,
        commit_sha,
        payload,
        prev_hash,
        hash,
    }
}

/// The chain root: the hash of the last event, or the genesis hash when empty.
pub fn root(events: &[Event]) -> String {
    events
        .last()
        .map(|e| e.hash.clone())
        .unwrap_or_else(|| GENESIS_PREV_HASH.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    Ok {
        root: String,
        count: usize,
    },
    /// The first place the chain stops adding up.
    Broken {
        seq: i64,
        reason: String,
        count: usize,
    },
}

impl Verification {
    pub fn is_ok(&self) -> bool {
        matches!(self, Verification::Ok { .. })
    }
}

/// Recompute the chain from scratch and report the first break.
pub fn verify(events: &[Event]) -> Verification {
    let count = events.len();
    let mut expected_prev = GENESIS_PREV_HASH.to_string();
    let mut expected_seq: i64 = 1;

    for event in events {
        if event.seq != expected_seq {
            return Verification::Broken {
                seq: event.seq,
                reason: format!("expected seq {expected_seq}, found {}", event.seq),
                count,
            };
        }
        if event.prev_hash != expected_prev {
            return Verification::Broken {
                seq: event.seq,
                reason: "prev_hash does not match the previous event".to_string(),
                count,
            };
        }
        let recomputed = event.expected_hash();
        if recomputed != event.hash {
            return Verification::Broken {
                seq: event.seq,
                reason: "hash does not match the event contents".to_string(),
                count,
            };
        }
        expected_prev = event.hash.clone();
        expected_seq += 1;
    }

    Verification::Ok {
        root: expected_prev,
        count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::now_ts;

    fn chain_of(n: usize) -> Vec<Event> {
        let mut events: Vec<Event> = Vec::new();
        for i in 0..n {
            let ev = next_event(
                events.last(),
                format!("id-{i}"),
                now_ts(),
                EventKind::Decision,
                None,
                serde_json::json!({"private": false, "text": format!("note {i}")}),
            );
            events.push(ev);
        }
        events
    }

    #[test]
    fn an_empty_chain_verifies_to_the_genesis_root() {
        assert_eq!(
            verify(&[]),
            Verification::Ok {
                root: GENESIS_PREV_HASH.to_string(),
                count: 0
            }
        );
        assert_eq!(root(&[]), GENESIS_PREV_HASH);
    }

    #[test]
    fn a_fresh_chain_verifies() {
        let events = chain_of(5);
        assert!(verify(&events).is_ok());
        assert_eq!(root(&events), events[4].hash);
        assert_eq!(events[0].prev_hash, GENESIS_PREV_HASH);
        assert_eq!(events[4].seq, 5);
    }

    #[test]
    fn editing_a_payload_breaks_the_chain_at_that_event() {
        let mut events = chain_of(5);
        events[2].payload = serde_json::json!({"private": false, "text": "tampered"});
        match verify(&events) {
            Verification::Broken { seq, .. } => assert_eq!(seq, 3),
            other => panic!("expected a break, got {other:?}"),
        }
    }

    #[test]
    fn removing_an_event_breaks_the_chain() {
        let mut events = chain_of(5);
        events.remove(2);
        match verify(&events) {
            Verification::Broken { seq, .. } => assert_eq!(seq, 4),
            other => panic!("expected a break, got {other:?}"),
        }
    }

    #[test]
    fn relinking_a_tampered_event_still_breaks_the_next_one() {
        let mut events = chain_of(4);
        events[1].payload = serde_json::json!({"private": true, "text": "tampered"});
        events[1].hash = events[1].expected_hash();
        match verify(&events) {
            Verification::Broken { seq, .. } => assert_eq!(seq, 3),
            other => panic!("expected a break, got {other:?}"),
        }
    }
}

use super::{
    load_notice_message, CompletedItemTracker, MAX_TRACKED_COMPLETED_ITEMS_PER_TURN,
    MAX_TRACKED_COMPLETED_ITEM_ID_BYTES,
};
use crate::persistence::LoadNotice;

#[test]
fn missing_preferences_are_a_quiet_first_run() {
    assert_eq!(load_notice_message(Some(LoadNotice::Missing)), None);
    assert!(load_notice_message(Some(LoadNotice::Corrupt)).is_some());
}

#[test]
fn completed_items_reset_for_every_local_turn_even_when_server_ids_repeat() {
    let mut tracker = CompletedItemTracker::default();
    tracker.begin_turn("thread", "turn-one");
    tracker.record("thread", "turn-one", "reused-item");
    tracker.observe_turn("thread", "turn-one");
    assert!(tracker.should_ignore("thread", "turn-one", "reused-item"));

    tracker.begin_turn("thread", "turn-one");
    assert!(!tracker.should_ignore("thread", "turn-one", "reused-item"));

    tracker.record("thread", "turn-one", "reused-item");
    tracker.begin_turn("thread", "turn-two");
    assert!(!tracker.should_ignore("thread", "turn-two", "reused-item"));
    assert!(!tracker.should_ignore("thread", "turn-one", "reused-item"));
}

#[test]
fn completed_item_tracking_saturates_closed_at_count_and_byte_bounds() {
    let mut tracker = CompletedItemTracker::default();
    tracker.begin_turn("thread", "count-bound");
    for index in 0..=MAX_TRACKED_COMPLETED_ITEMS_PER_TURN {
        tracker.record("thread", "count-bound", &format!("item-{index}"));
    }
    assert_eq!(tracker.ids.len(), MAX_TRACKED_COMPLETED_ITEMS_PER_TURN);
    assert!(tracker.should_ignore("thread", "count-bound", "untracked-late-item"));
    assert!(!tracker.should_ignore("other-thread", "count-bound", "untracked-late-item"));

    tracker.begin_turn("thread", "byte-bound");
    let at_limit = "x".repeat(MAX_TRACKED_COMPLETED_ITEM_ID_BYTES);
    tracker.record("thread", "byte-bound", &at_limit);
    tracker.record("thread", "byte-bound", "over-limit");
    assert_eq!(tracker.ids.len(), 1);
    assert_eq!(tracker.id_bytes, MAX_TRACKED_COMPLETED_ITEM_ID_BYTES);
    assert!(tracker.should_ignore("thread", "byte-bound", "another-untracked-item"));

    tracker.begin_turn("thread", "fresh-turn");
    assert!(!tracker.should_ignore("thread", "fresh-turn", "new-item"));
}

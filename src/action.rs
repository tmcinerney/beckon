use std::time::{Duration, Instant};

/// State for the explicitly enabled repeat-press confirmation action.
///
/// A confirmation is only armed after a successful focus. It additionally
/// requires the same pane to still be focused, so a later key press cannot
/// inject Enter into a pane the user has left.
#[derive(Debug, Default)]
pub struct RepeatPressConfirm {
    pending: Option<PendingConfirm>,
}

#[derive(Debug)]
struct PendingConfirm {
    key: String,
    pane_id: String,
    expires_at: Instant,
}

impl RepeatPressConfirm {
    pub fn take_if_ready(
        &mut self,
        key: &str,
        pane_id: &str,
        pane_is_focused: bool,
        now: Instant,
    ) -> bool {
        let ready = self.pending.as_ref().is_some_and(|pending| {
            pending.key == key
                && pending.pane_id == pane_id
                && now <= pending.expires_at
                && pane_is_focused
        });
        if ready {
            self.pending = None;
        }
        ready
    }

    pub fn arm(&mut self, key: &str, pane_id: &str, window: Duration, now: Instant) {
        self.pending = Some(PendingConfirm {
            key: key.to_owned(),
            pane_id: pane_id.to_owned(),
            expires_at: now + window,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirms_only_the_same_focused_pane_within_the_window() {
        let now = Instant::now();
        let mut confirm = RepeatPressConfirm::default();
        confirm.arm("f1", "w:p1", Duration::from_millis(750), now);

        assert!(!confirm.take_if_ready("f1", "w:p1", false, now + Duration::from_millis(1)));
        assert!(confirm.take_if_ready("f1", "w:p1", true, now + Duration::from_millis(2)));
        assert!(!confirm.take_if_ready("f1", "w:p1", true, now + Duration::from_millis(3)));
    }

    #[test]
    fn expires_and_rejects_other_keys_or_panes() {
        let now = Instant::now();
        let mut confirm = RepeatPressConfirm::default();
        confirm.arm("f1", "w:p1", Duration::from_millis(10), now);

        assert!(!confirm.take_if_ready("f2", "w:p1", true, now));
        assert!(!confirm.take_if_ready("f1", "w:p2", true, now));
        assert!(!confirm.take_if_ready("f1", "w:p1", true, now + Duration::from_millis(11)));
    }
}

use std::time::{Duration, Instant};

use crate::core::PaneRef;

/// State for the explicitly enabled repeat-press confirmation action.
///
/// A confirmation is only armed after a successful focus. It additionally
/// requires the same pane to still be focused, so a later key press cannot
/// inject Enter into a pane the user has left. The pane reference carries its
/// session so identical pane IDs in two sessions cannot cross-confirm.
#[derive(Debug, Default)]
pub struct RepeatPressConfirm {
    pending: Option<PendingConfirm>,
}

#[derive(Debug)]
struct PendingConfirm {
    key: String,
    pane: PaneRef,
    expires_at: Instant,
}

impl RepeatPressConfirm {
    pub fn take_if_ready(
        &mut self,
        key: &str,
        pane: &PaneRef,
        pane_is_focused: bool,
        now: Instant,
    ) -> bool {
        let ready = self.pending.as_ref().is_some_and(|pending| {
            pending.key == key
                && &pending.pane == pane
                && now <= pending.expires_at
                && pane_is_focused
        });
        if ready {
            self.pending = None;
        }
        ready
    }

    pub fn arm(&mut self, key: &str, pane: &PaneRef, window: Duration, now: Instant) {
        self.pending = Some(PendingConfirm {
            key: key.to_owned(),
            pane: pane.clone(),
            expires_at: now + window,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(session: &str, pane_id: &str) -> PaneRef {
        PaneRef::new(session, pane_id)
    }

    #[test]
    fn confirms_only_the_same_focused_pane_within_the_window() {
        let now = Instant::now();
        let mut confirm = RepeatPressConfirm::default();
        let pane = reference("default", "w:p1");
        confirm.arm("f1", &pane, Duration::from_millis(750), now);

        assert!(!confirm.take_if_ready("f1", &pane, false, now + Duration::from_millis(1)));
        assert!(confirm.take_if_ready("f1", &pane, true, now + Duration::from_millis(2)));
        assert!(!confirm.take_if_ready("f1", &pane, true, now + Duration::from_millis(3)));
    }

    #[test]
    fn expires_and_rejects_other_keys_panes_or_sessions() {
        let now = Instant::now();
        let mut confirm = RepeatPressConfirm::default();
        let pane = reference("default", "w:p1");
        confirm.arm("f1", &pane, Duration::from_millis(10), now);

        assert!(!confirm.take_if_ready("f2", &pane, true, now));
        assert!(!confirm.take_if_ready("f1", &reference("default", "w:p2"), true, now));
        assert!(!confirm.take_if_ready("f1", &reference("agent-workspace", "w:p1"), true, now));
        assert!(!confirm.take_if_ready("f1", &pane, true, now + Duration::from_millis(11)));
    }
}

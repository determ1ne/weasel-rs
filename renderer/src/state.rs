use weasel_common::message::RenderSnapshot;

pub type Owner = u64;
// Matches common InputState's candidate limit; retain separate text/geometry bounds.
const MAX_ITEMS: usize = 256;

/// A newer connection may take ownership; a superseded connection cannot reclaim it.
#[derive(Default)]
pub struct Mailbox {
    pub owner: Option<Owner>,
    newest_owner: Owner,
    pub pending: Option<(Owner, Option<RenderSnapshot>)>,
    pub wake_pending: bool,
    pub closed: bool,
    // Server presentation order spans every session, token, show and hide.
    // Retain this after consuming pending; reset only on a newer owner.
    latest_sequence: u64,
}

impl Mailbox {
    pub fn schedule_wake(&mut self) -> bool {
        if self.wake_pending {
            return false;
        }
        self.wake_pending = true;
        true
    }

    pub fn take_pending(&mut self) -> Option<(Owner, Option<RenderSnapshot>)> {
        self.wake_pending = false;
        self.pending.take()
    }
    pub fn render(&mut self, owner: Owner, snapshot: RenderSnapshot) -> bool {
        if self.closed
            || owner < self.newest_owner
            || snapshot.sequence == 0
            || (owner == self.newest_owner
                && (self.owner != Some(owner) || snapshot.sequence <= self.latest_sequence))
        {
            return false;
        }
        self.latest_sequence = snapshot.sequence;
        self.newest_owner = owner;
        self.owner = Some(owner);
        self.pending = Some((owner, Some(snapshot)));
        true
    }

    pub fn disconnect(&mut self, owner: Owner) -> bool {
        if self.closed || self.owner != Some(owner) {
            return false;
        }
        self.owner = None;
        self.pending = Some((owner, None));
        true
    }
}

pub fn validate(snapshot: &RenderSnapshot) -> Result<(), String> {
    if let Some(preedit) = &snapshot.preedit {
        crate::theme_api::Preedit {
            text: preedit.text.clone(),
            cursor: preedit.cursor_utf16,
        }
        .validate()?;
    }
    if snapshot.items.len() > MAX_ITEMS {
        return Err("snapshot exceeds 256 items".into());
    }
    let mut bytes = 0;
    for item in &snapshot.items {
        for text in [&item.primary_text, &item.secondary_text, &item.kind] {
            if text.len() > 4096 || text.contains('\0') {
                return Err("snapshot text exceeds 4096 bytes or contains NUL".into());
            }
            bytes += text.len();
        }
    }
    if bytes > 65536 {
        return Err("snapshot text exceeds 64 KiB".into());
    }
    if (snapshot.items.is_empty() && snapshot.selected_index != 0)
        || (!snapshot.items.is_empty() && snapshot.selected_index as usize >= snapshot.items.len())
        || snapshot
            .page_start
            .checked_add(snapshot.items.len() as u32)
            .is_none()
        || snapshot.total_item_count.is_some_and(|total| {
            snapshot
                .page_start
                .saturating_add(snapshot.items.len() as u32)
                > total
        })
    {
        return Err("invalid snapshot index/count".into());
    }
    if let Some(anchor) = &snapshot.anchor {
        if anchor.valid
            && (anchor.left > anchor.right
                || anchor.top > anchor.bottom
                || [anchor.left, anchor.right, anchor.top, anchor.bottom]
                    .iter()
                    .any(|v| !(-1_000_000..=1_000_000).contains(v)))
        {
            return Err("invalid snapshot anchor".into());
        }
    }
    Ok(())
}

pub fn same_content(a: &RenderSnapshot, b: &RenderSnapshot) -> bool {
    a.session_id == b.session_id
        && a.preedit == b.preedit
        && a.token == b.token
        && a.revision == b.revision
        && a.items == b.items
        && a.selected_index == b.selected_index
        && a.page_start == b.page_start
        && a.total_item_count == b.total_item_count
        && a.can_page_previous == b.can_page_previous
        && a.can_page_next == b.can_page_next
}

#[cfg(test)]
mod tests {
    use super::*;
    use weasel_common::message::ContextToken;

    fn snapshot() -> RenderSnapshot {
        RenderSnapshot {
            visible: true,
            sequence: 1,
            ..Default::default()
        }
    }

    fn versioned(
        sequence: u64,
        session: u64,
        context: u64,
        generation: u64,
        revision: u64,
    ) -> RenderSnapshot {
        RenderSnapshot {
            sequence,
            session_id: session,
            token: Some(ContextToken {
                context_id: context,
                connection_epoch: 1,
                generation,
            }),
            revision,
            ..snapshot()
        }
    }

    #[test]
    fn consumed_snapshots_still_reject_lower_or_equal_sequence() {
        let mut mailbox = Mailbox::default();
        assert!(mailbox.render(1, versioned(10, 1, 1, 3, 10)));
        mailbox.take_pending();
        assert!(!mailbox.render(1, versioned(9, 2, 2, 100, 100)));
        assert!(!mailbox.render(1, versioned(10, 1, 1, 4, 100)));
        assert!(!mailbox.render(1, versioned(0, 1, 1, 4, 100)));
        assert!(mailbox.pending.is_none());
        assert!(mailbox.render(1, versioned(11, 1, 1, 3, 10))); // Layout-only update.
        assert!(mailbox.render(1, versioned(12, 1, 1, 1, 1))); // Publisher is authoritative.
    }

    #[test]
    fn show_a_hide_b_and_reenter_a_with_unchanged_generation() {
        let mut mailbox = Mailbox::default();
        assert!(mailbox.render(1, versioned(1, 1, 1, 1, 5)));
        let mut hidden = versioned(2, 2, 2, 1, 1);
        hidden.visible = false;
        assert!(mailbox.render(1, hidden.clone()));
        assert!(!mailbox.take_pending().unwrap().1.unwrap().visible);
        assert!(mailbox.render(1, versioned(3, 1, 1, 1, 6)));
        assert!(!mailbox.render(1, hidden));
        let current = mailbox.take_pending().unwrap().1.unwrap();
        assert!(current.visible);
        assert_eq!(current.session_id, 1);
        assert_eq!(current.token.unwrap().generation, 1);
    }

    #[test]
    fn cancel_hide_accepts_new_generation_and_rejects_replayed_show() {
        let mut mailbox = Mailbox::default();
        assert!(mailbox.render(1, versioned(1, 1, 1, 3, 10)));
        let mut hidden = versioned(2, 1, 1, 4, 11);
        hidden.visible = false;
        assert!(mailbox.render(1, hidden));
        assert!(!mailbox.render(1, versioned(1, 1, 1, 3, 10)));
        assert!(!mailbox.render(1, versioned(2, 1, 1, 4, 12)));
        assert!(
            !mailbox
                .pending
                .as_ref()
                .unwrap()
                .1
                .as_ref()
                .unwrap()
                .visible
        );
        assert!(mailbox.disconnect(1));
    }

    #[test]
    fn new_owner_resets_sequence_and_can_start_with_hide() {
        let mut mailbox = Mailbox::default();
        assert!(!mailbox.render(1, versioned(0, 1, 1, 1, 1)));
        assert!(mailbox.render(1, versioned(u64::MAX, 1, 1, 1, 1)));
        let hidden = RenderSnapshot {
            visible: false,
            ..snapshot()
        };
        assert!(mailbox.render(2, hidden));
        assert_eq!(mailbox.latest_sequence, 1);
        assert!(!mailbox.disconnect(1));
        assert!(!mailbox.render(1, versioned(u64::MAX, 1, 1, 1, 1)));
        assert!(mailbox.disconnect(2));
        assert!(!mailbox.render(2, versioned(2, 1, 1, 1, 1)));
    }

    #[test]
    fn ownership_and_disconnect_are_ordered() {
        let mut mailbox = Mailbox::default();
        assert!(mailbox.render(1, snapshot()));
        assert!(mailbox.render(2, snapshot()));
        assert!(!mailbox.disconnect(1));
        assert!(!mailbox.render(1, RenderSnapshot::default()));
        assert!(!mailbox.render(1, snapshot()));
        assert_eq!(mailbox.owner, Some(2));
        assert!(mailbox.disconnect(2));
        assert!(mailbox.pending.as_ref().unwrap().1.is_none());
    }

    #[test]
    fn latest_snapshot_replaces_backlog() {
        let mut mailbox = Mailbox::default();
        let mut wakes = 0;
        for revision in 0..10000 {
            mailbox.render(
                1,
                RenderSnapshot {
                    revision,
                    sequence: revision + 1,
                    ..snapshot()
                },
            );
            wakes += usize::from(mailbox.schedule_wake());
        }
        assert_eq!(wakes, 1);
        assert_eq!(mailbox.take_pending().unwrap().1.unwrap().revision, 9999);
        assert!(mailbox.pending.is_none());
        assert!(mailbox.render(
            1,
            RenderSnapshot {
                revision: 10000,
                sequence: 10001,
                ..snapshot()
            }
        ));
        assert!(mailbox.schedule_wake());
    }

    #[test]
    fn disconnect_hide_is_replaced_by_new_owner_and_close_is_terminal() {
        let mut mailbox = Mailbox::default();
        mailbox.render(1, snapshot());
        mailbox.disconnect(1);
        mailbox.render(2, snapshot());
        let (owner, pending) = mailbox.take_pending().unwrap();
        assert_eq!(owner, 2);
        assert!(pending.unwrap().visible);
        mailbox.closed = true;
        assert!(!mailbox.render(3, snapshot()));
        assert!(!mailbox.disconnect(2));
    }

    #[test]
    fn aggregate_text_and_page_bounds_are_checked() {
        use weasel_common::message::RenderItem;
        let mut s = snapshot();
        s.items = vec![
            RenderItem {
                primary_text: "x".repeat(4096),
                ..Default::default()
            };
            17
        ];
        assert!(validate(&s).is_err());
        s.items.truncate(1);
        s.page_start = u32::MAX;
        assert!(validate(&s).is_err());
        s.page_start = 10;
        s.total_item_count = Some(10);
        assert!(validate(&s).is_err());
        s.total_item_count = Some(11);
        assert!(validate(&s).is_ok());
    }

    #[test]
    fn rejects_bad_payloads() {
        use weasel_common::message::{RenderItem, RenderRect};
        let mut s = snapshot();
        s.items = vec![RenderItem::default(); 257];
        assert!(validate(&s).is_err());
        s.items.truncate(1);
        s.items[0].primary_text = "x".repeat(4097);
        assert!(validate(&s).is_err());
        s.items[0].primary_text = "\0".into();
        assert!(validate(&s).is_err());
        s.items[0].primary_text.clear();
        s.selected_index = 1;
        assert!(validate(&s).is_err());
        s.selected_index = 0;
        s.anchor = Some(RenderRect {
            valid: true,
            left: i32::MIN,
            ..Default::default()
        });
        assert!(validate(&s).is_err());
        s.anchor = None;
        assert!(validate(&s).is_ok());
    }

    #[test]
    fn layout_only_retains_content_but_identity_changes_do_not() {
        let a = snapshot();
        let mut b = a.clone();
        b.anchor = Some(Default::default());
        b.sequence += 1;
        assert!(same_content(&a, &b));
        b.revision += 1;
        assert!(!same_content(&a, &b));
    }

    #[test]
    fn accepts_256_candidates_and_last_selection_but_rejects_257() {
        let mut s = snapshot();
        s.items = vec![weasel_common::message::RenderItem::default(); 256];
        s.selected_index = 255;
        s.total_item_count = Some(256);
        assert!(validate(&s).is_ok());
        s.selected_index = 256;
        assert!(validate(&s).is_err());
        s.selected_index = 255;
        s.items.push(Default::default());
        s.total_item_count = Some(257);
        assert!(validate(&s).is_err());
    }

    #[test]
    fn unknown_total_is_preserved_and_distinct_from_known_zero() {
        let mut s = snapshot();
        s.items.push(Default::default());
        s.page_start = 10;
        assert_eq!(s.total_item_count, None);
        assert!(validate(&s).is_ok());
        let mut mailbox = Mailbox::default();
        assert!(mailbox.render(1, s.clone()));
        assert_eq!(
            mailbox.take_pending().unwrap().1.unwrap().total_item_count,
            None
        );
        s.total_item_count = Some(0);
        assert!(validate(&s).is_err());
        s.items.clear();
        assert!(validate(&s).is_err());
        s.page_start = 0;
        assert!(validate(&s).is_ok());
    }
}

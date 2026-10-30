//! Desktop thread traversal follows the visible sidebar, without wrapping.

#[derive(Clone, Copy)]
pub enum Destination {
    Previous,
    Next,
    Index(usize),
}

pub fn target<T: PartialEq>(
    threads: &[T],
    current: Option<&T>,
    destination: Destination,
) -> Option<usize> {
    match destination {
        Destination::Index(index) => (index < threads.len()).then_some(index),
        Destination::Previous | Destination::Next => {
            let Some(current) = current else {
                return match destination {
                    Destination::Previous => threads.len().checked_sub(1),
                    _ => (!threads.is_empty()).then_some(0),
                };
            };
            let index = threads.iter().position(|thread| thread == current)?;
            match destination {
                Destination::Previous => index.checked_sub(1),
                _ => (index + 1 < threads.len()).then_some(index + 1),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_follows_sidebar_order_and_stops_at_the_ends() {
        let threads = ["pinned", "active", "snoozed", "settled"];
        assert_eq!(
            target(&threads, Some(&"active"), Destination::Previous),
            Some(0)
        );
        assert_eq!(
            target(&threads, Some(&"active"), Destination::Next),
            Some(2)
        );
        assert_eq!(
            target(&threads, Some(&"pinned"), Destination::Previous),
            None
        );
        assert_eq!(target(&threads, Some(&"settled"), Destination::Next), None);
        assert_eq!(target(&threads, Some(&"hidden"), Destination::Next), None);
    }

    #[test]
    fn drafts_enter_at_the_corresponding_end_and_empty_lists_do_nothing() {
        let threads = ["first", "last"];
        assert_eq!(target(&threads, None, Destination::Next), Some(0));
        assert_eq!(target(&threads, None, Destination::Previous), Some(1));
        for destination in [
            Destination::Previous,
            Destination::Next,
            Destination::Index(0),
        ] {
            assert_eq!(target::<&str>(&[], None, destination), None);
        }
    }

    #[test]
    fn numbered_jumps_use_visible_positions_even_without_a_selected_thread() {
        let threads = ["pinned", "active"];
        assert_eq!(target(&threads, None, Destination::Index(0)), Some(0));
        assert_eq!(
            target(&threads, Some(&"hidden"), Destination::Index(1)),
            Some(1)
        );
        assert_eq!(target(&threads, None, Destination::Index(8)), None);
    }
}

/// Conversation history is independent of the sidebar's changing sort order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    Thread(String),
    Draft(String),
}

#[derive(Default)]
pub struct History {
    entries: Vec<Location>,
    cursor: usize,
}

impl History {
    pub fn visit(&mut self, location: Location) {
        if self.entries.get(self.cursor) == Some(&location) {
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(location);
        // Bound memory even in windows that stay open for weeks.
        if self.entries.len() > 100 {
            self.entries.remove(0);
        }
        self.cursor = self.entries.len() - 1;
    }

    pub fn travel(
        &mut self,
        forward: bool,
        available: impl Fn(&Location) -> bool,
    ) -> Option<Location> {
        let mut index = self.cursor;
        loop {
            index = if forward {
                index
                    .checked_add(1)
                    .filter(|index| *index < self.entries.len())?
            } else {
                index.checked_sub(1)?
            };
            if available(&self.entries[index]) {
                self.cursor = index;
                return Some(self.entries[index].clone());
            }
        }
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    fn thread(id: &str) -> Location {
        Location::Thread(id.into())
    }

    #[test]
    fn back_and_forward_restore_visits_instead_of_sidebar_order() {
        let mut history = History::default();
        assert_eq!(history.travel(false, |_| true), None);
        history.visit(thread("b"));
        history.visit(Location::Draft("draft".into()));
        history.visit(thread("a"));
        assert_eq!(
            history.travel(false, |_| true),
            Some(Location::Draft("draft".into()))
        );
        // Opening the destination must not erase the forward branch.
        history.visit(Location::Draft("draft".into()));
        assert_eq!(history.travel(true, |_| true), Some(thread("a")));
        assert_eq!(history.travel(true, |_| true), None);
        assert_eq!(
            history.travel(false, |_| true),
            Some(Location::Draft("draft".into()))
        );
        assert_eq!(history.travel(false, |_| true), Some(thread("b")));
        assert_eq!(history.travel(false, |_| true), None);
    }

    #[test]
    fn a_new_visit_replaces_the_forward_branch_and_duplicate_visits_do_nothing() {
        let mut history = History::default();
        history.visit(thread("a"));
        history.visit(thread("b"));
        history.visit(thread("b"));
        assert_eq!(history.travel(false, |_| true), Some(thread("a")));
        history.visit(thread("c"));
        assert_eq!(history.travel(true, |_| true), None);
        assert_eq!(history.travel(false, |_| true), Some(thread("a")));
    }

    #[test]
    fn removed_destinations_are_skipped_without_losing_the_current_position() {
        let mut history = History::default();
        history.visit(thread("a"));
        history.visit(Location::Draft("sent".into()));
        history.visit(thread("b"));
        let available = |location: &Location| matches!(location, Location::Thread(_));
        assert_eq!(history.travel(false, available), Some(thread("a")));
        assert_eq!(history.travel(true, available), Some(thread("b")));
        assert_eq!(history.travel(false, |_| false), None);
        assert_eq!(history.travel(false, available), Some(thread("a")));
    }

    #[test]
    fn history_is_bounded() {
        let mut history = History::default();
        for index in 0..150 {
            history.visit(thread(&index.to_string()));
        }
        for _ in 0..99 {
            assert!(history.travel(false, |_| true).is_some());
        }
        assert_eq!(history.travel(false, |_| true), None);
        assert_eq!(history.entries[0], thread("50"));
    }
}

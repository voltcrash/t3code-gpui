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

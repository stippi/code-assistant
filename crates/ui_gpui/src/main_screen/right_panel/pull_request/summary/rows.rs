//! The Summary tab as rows of one virtualized list: the meta rows, the
//! description, the checks, then the conversation, the newest ten
//! conversations first and older ones ten at a time.

use super::super::conversation::logic::page;
use gpui_kit::{Pixels, SharedString};

/// Conversations shown at first, and added per "Show older".
pub const PAGE: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// Reviewers and labels.
    Meta,
    Description,
    Checks,
    /// "Comments (N)" with the filter and the order.
    CommentsHeader,
    /// Loading, nothing to show, or the conversation read only in part.
    CommentsNote,
    /// Shows up to [`PAGE`] more of the `hidden` older conversations.
    ShowOlder {
        hidden: usize,
    },
    /// A conversation item by id.
    Item(SharedString),
    /// Back to the newest [`PAGE`].
    ShowRecent,
}

/// How the conversation part is laid out.
pub struct CommentsLayout<'a> {
    /// The items the filter lets through, oldest first.
    pub ids: &'a [SharedString],
    pub newest_first: bool,
    /// Conversations to show at most.
    pub count: usize,
    /// The Comments section is unfolded.
    pub open: bool,
    /// There is something to say above the items.
    pub note: bool,
}

pub fn summary_rows(layout: CommentsLayout) -> Vec<Row> {
    let mut rows = vec![
        Row::Meta,
        Row::Description,
        Row::Checks,
        Row::CommentsHeader,
    ];
    if !layout.open {
        return rows;
    }
    if layout.note {
        rows.push(Row::CommentsNote);
    }
    let (range, hidden) = page(layout.ids.len(), layout.count);
    let items = layout.ids[range].iter().cloned().map(Row::Item);
    let older = (hidden > 0).then_some(Row::ShowOlder { hidden });
    if layout.newest_first {
        rows.extend(items.rev());
        rows.extend(older);
    } else {
        rows.extend(older);
        rows.extend(items);
    }
    if layout.count > PAGE {
        rows.push(Row::ShowRecent);
    }
    rows
}

/// The section header to pin above the list while its section scrolls
/// under the top edge (`top`: the row there and how far it is scrolled
/// into it): Description and Checks are one row each, the Comments header
/// heads every row after it.
pub fn pinned_section(rows: &[Row], top: usize, offset: Pixels) -> Option<Row> {
    let row = rows.get(top)?;
    let scrolled = offset > Pixels::ZERO;
    match row {
        Row::Description | Row::Checks | Row::CommentsHeader if scrolled => Some(row.clone()),
        Row::Meta | Row::Description | Row::Checks | Row::CommentsHeader => None,
        _ => Some(Row::CommentsHeader),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<SharedString> {
        (0..n).map(|i| SharedString::from(i.to_string())).collect()
    }

    fn shown(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .skip(4)
            .map(|row| match row {
                Row::Item(id) => id.to_string(),
                Row::ShowOlder { hidden } => format!("older {hidden}"),
                Row::ShowRecent => "recent".into(),
                Row::CommentsNote => "note".into(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn the_newest_ten_show_first() {
        let ids = ids(12);
        let rows = summary_rows(CommentsLayout {
            ids: &ids,
            newest_first: true,
            count: PAGE,
            open: true,
            note: false,
        });
        assert_eq!(
            rows[..4],
            [
                Row::Meta,
                Row::Description,
                Row::Checks,
                Row::CommentsHeader
            ]
        );
        assert_eq!(
            shown(&rows),
            [
                "11", "10", "9", "8", "7", "6", "5", "4", "3", "2", "older 2"
            ]
        );
    }

    #[test]
    fn oldest_first_puts_the_older_ones_on_top() {
        let ids = ids(12);
        let rows = summary_rows(CommentsLayout {
            ids: &ids,
            newest_first: false,
            count: PAGE,
            open: true,
            note: true,
        });
        assert_eq!(&shown(&rows)[..3], ["note", "older 2", "2"]);
        assert_eq!(shown(&rows).last().unwrap(), "11");
    }

    #[test]
    fn more_pages_can_go_back_to_the_recent() {
        let ids = ids(12);
        let rows = summary_rows(CommentsLayout {
            ids: &ids,
            newest_first: true,
            count: 2 * PAGE,
            open: true,
            note: false,
        });
        let shown = shown(&rows);
        assert_eq!(shown.len(), 13);
        assert_eq!(shown.last().unwrap(), "recent");
    }

    #[test]
    fn folded_comments_show_no_items() {
        let ids = ids(3);
        let rows = summary_rows(CommentsLayout {
            ids: &ids,
            newest_first: true,
            count: PAGE,
            open: false,
            note: true,
        });
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn a_section_scrolled_under_the_top_pins_its_header() {
        use gpui_kit::px;
        let ids = ids(3);
        let rows = summary_rows(CommentsLayout {
            ids: &ids,
            newest_first: true,
            count: PAGE,
            open: true,
            note: false,
        });
        assert_eq!(pinned_section(&rows, 0, px(30.)), None);
        assert_eq!(pinned_section(&rows, 1, px(0.)), None, "its header shows");
        assert_eq!(pinned_section(&rows, 1, px(5.)), Some(Row::Description));
        assert_eq!(pinned_section(&rows, 2, px(5.)), Some(Row::Checks));
        assert_eq!(pinned_section(&rows, 3, px(0.)), None);
        assert_eq!(pinned_section(&rows, 3, px(1.)), Some(Row::CommentsHeader));
        assert_eq!(pinned_section(&rows, 5, px(0.)), Some(Row::CommentsHeader));
        assert_eq!(pinned_section(&rows, 99, px(0.)), None);
    }
}

//! Which repository the pull request list shows, for a session whose folder
//! holds several checkouts: the first focus, the order and the rows of the
//! shared [`RepoPicker`](crate::main_screen::right_panel::repo_picker).

use crate::main_screen::right_panel::repo_picker::PickerRepo;
use code_assistant_core::pull_requests::{RepoCheckout, SessionCheckouts};
use github::RepoRef;

/// The picker's row of `checkout`.
pub fn picker_repo(checkout: &RepoCheckout) -> PickerRepo<RepoRef> {
    let repo = &checkout.repo;
    let place = match repo.host.as_str() {
        "github.com" => repo.owner.clone(),
        host => format!("{} · {host}", repo.owner),
    };
    let directory = checkout
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    PickerRepo {
        key: repo.clone(),
        name: repo.name.clone(),
        place: Some(place),
        branch: checkout.branch.clone(),
        note: None,
        search: std::iter::once(repo.full_name()).chain(directory).collect(),
    }
}

/// The repository a session lists at first: the one it listed last, else
/// the one of its folder itself (as without any others), else the one last
/// chosen in the folder, else the only one. `None` leaves the choice open.
pub fn first_focus<'a>(
    found: &'a SessionCheckouts,
    listed_last: Option<&RepoRef>,
    recent: &[RepoRef],
) -> Option<&'a RepoCheckout> {
    let checkouts = &found.checkouts;
    let of = |repo: &RepoRef| checkouts.iter().find(|checkout| checkout.repo == *repo);
    listed_last
        .and_then(of)
        .or_else(|| checkouts.iter().find(|c| c.path == found.directory))
        .or_else(|| recent.iter().find_map(of))
        .or(match checkouts.as_slice() {
            [only] => Some(only),
            _ => None,
        })
}

/// The ones chosen recently first, the others as they came.
pub fn recent_first(mut checkouts: Vec<RepoCheckout>, recent: &[RepoRef]) -> Vec<RepoCheckout> {
    checkouts.sort_by_key(|checkout| {
        recent
            .iter()
            .position(|repo| *repo == checkout.repo)
            .unwrap_or(recent.len())
    });
    checkouts
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::main_screen::right_panel::repo_picker::matches;
    use std::path::PathBuf;

    fn repo(name: &str) -> RepoRef {
        RepoRef::new("github.com", "team", name)
    }

    fn checkout(name: &str) -> RepoCheckout {
        RepoCheckout {
            path: PathBuf::from("/folder").join(name),
            repo: repo(name),
            branch: None,
        }
    }

    fn found(checkouts: Vec<RepoCheckout>) -> SessionCheckouts {
        SessionCheckouts {
            directory: PathBuf::from("/folder"),
            checkouts,
        }
    }

    fn focus(found: &SessionCheckouts, last: Option<&str>, recent: &[&str]) -> Option<String> {
        let last = last.map(repo);
        let recent: Vec<RepoRef> = recent.iter().map(|name| repo(name)).collect();
        first_focus(found, last.as_ref(), &recent).map(|c| c.repo.name.clone())
    }

    #[test]
    fn a_folder_of_several_leaves_the_first_choice_open() {
        let several = found(vec![checkout("a"), checkout("b")]);
        assert_eq!(focus(&several, None, &[]), None);
        assert_eq!(focus(&several, None, &["gone"]), None);
    }

    #[test]
    fn what_was_listed_last_comes_back_then_the_folders_last_choice() {
        let several = found(vec![checkout("a"), checkout("b"), checkout("c")]);
        assert_eq!(focus(&several, Some("b"), &["c"]).as_deref(), Some("b"));
        assert_eq!(focus(&several, None, &["gone", "c"]).as_deref(), Some("c"));
        assert_eq!(focus(&several, Some("gone"), &[]), None);
    }

    #[test]
    fn a_folder_that_is_a_repository_itself_lists_that_one_first() {
        let root = RepoCheckout {
            path: PathBuf::from("/folder"),
            ..checkout("root")
        };
        let found = found(vec![root, checkout("nested")]);
        assert_eq!(focus(&found, None, &["nested"]).as_deref(), Some("root"));
        assert_eq!(
            focus(&found, Some("nested"), &[]).as_deref(),
            Some("nested")
        );
    }

    #[test]
    fn a_single_checkout_is_the_focus() {
        assert_eq!(
            focus(&found(vec![checkout("a")]), None, &[]).as_deref(),
            Some("a")
        );
    }

    #[test]
    fn recent_ones_come_first_the_others_as_they_came() {
        let ordered = recent_first(
            vec![checkout("a"), checkout("b"), checkout("c"), checkout("d")],
            &[repo("c"), repo("gone"), repo("b")],
        );
        let names: Vec<_> = ordered.iter().map(|c| c.repo.name.as_str()).collect();
        assert_eq!(names, ["c", "b", "a", "d"]);
    }

    #[test]
    fn the_search_matches_names_owner_and_branch() {
        let tool = RepoCheckout {
            path: PathBuf::from("/folder/checked-out-as"),
            branch: Some("feature/x".into()),
            ..checkout("tool")
        };
        for query in ["", " TOOL ", "team/", "checked-out", "feature"] {
            assert!(matches(&picker_repo(&tool), query), "{query}");
        }
        assert!(!matches(&picker_repo(&tool), "other"));
    }
}

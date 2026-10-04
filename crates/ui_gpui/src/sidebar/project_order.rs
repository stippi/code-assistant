//! The stable order of projects in the sidebar and the project picker.
//!
//! Projects do not move by activity. A project seen for the first time goes
//! to the top; after that only the user moves it, by dragging its folder.
//! The order is stored in the UI settings and may name projects that are
//! gone; they are skipped when displayed and keep their place should they
//! come back.

use std::collections::HashSet;
use std::time::SystemTime;

/// Put the projects not yet in `order` at its top, the most recently active
/// first. Returns whether `order` changed.
pub fn adopt_new_projects(order: &mut Vec<String>, known: &[(String, SystemTime)]) -> bool {
    let mut new: Vec<&(String, SystemTime)> = known
        .iter()
        .filter(|(name, _)| !order.contains(name))
        .collect();
    if new.is_empty() {
        return false;
    }
    new.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    order.splice(0..0, new.into_iter().map(|(name, _)| name.clone()));
    true
}

/// The known projects in stored order.
pub fn displayed<'a>(order: &'a [String], known: &HashSet<&str>) -> Vec<&'a str> {
    order
        .iter()
        .map(String::as_str)
        .filter(|name| known.contains(name))
        .collect()
}

/// Move `dragged` to where `target` is: dragged upward it lands above the
/// target, dragged downward below it. Returns whether `order` changed.
pub fn move_project(order: &mut Vec<String>, dragged: &str, target: &str) -> bool {
    let (Some(from), Some(to)) = (
        order.iter().position(|name| name == dragged),
        order.iter().position(|name| name == target),
    ) else {
        return false;
    };
    if from == to {
        return false;
    }
    let project = order.remove(from);
    order.insert(to, project);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn names(order: &[String]) -> Vec<&str> {
        order.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_first_start_orders_projects_by_activity() {
        let mut order = Vec::new();
        let known = [
            ("old".to_string(), at(1)),
            ("recent".to_string(), at(3)),
            ("middle".to_string(), at(2)),
        ];
        assert!(adopt_new_projects(&mut order, &known));
        assert_eq!(names(&order), ["recent", "middle", "old"]);
    }

    #[test]
    fn known_projects_keep_their_place_whatever_their_activity() {
        let mut order = vec!["a".to_string(), "b".to_string()];
        let known = [("a".to_string(), at(1)), ("b".to_string(), at(9))];
        assert!(!adopt_new_projects(&mut order, &known));
        assert_eq!(names(&order), ["a", "b"]);
    }

    #[test]
    fn a_new_project_goes_to_the_top() {
        let mut order = vec!["a".to_string(), "b".to_string()];
        let known = [
            ("a".to_string(), at(5)),
            ("b".to_string(), at(5)),
            ("new".to_string(), at(1)),
        ];
        assert!(adopt_new_projects(&mut order, &known));
        assert_eq!(names(&order), ["new", "a", "b"]);
    }

    #[test]
    fn projects_that_are_gone_keep_their_place_but_are_not_displayed() {
        let order = vec!["a".to_string(), "gone".to_string(), "b".to_string()];
        let known: HashSet<&str> = ["a", "b"].into();
        assert_eq!(displayed(&order, &known), ["a", "b"]);
    }

    #[test]
    fn dragging_upward_lands_above_the_target() {
        let mut order: Vec<String> = ["a", "b", "c", "d"].map(String::from).into();
        assert!(move_project(&mut order, "d", "b"));
        assert_eq!(names(&order), ["a", "d", "b", "c"]);
    }

    #[test]
    fn dragging_downward_lands_below_the_target() {
        let mut order: Vec<String> = ["a", "b", "c", "d"].map(String::from).into();
        assert!(move_project(&mut order, "a", "c"));
        assert_eq!(names(&order), ["b", "c", "a", "d"]);
    }

    #[test]
    fn dropping_on_itself_or_an_unknown_project_changes_nothing() {
        let mut order: Vec<String> = ["a", "b"].map(String::from).into();
        assert!(!move_project(&mut order, "a", "a"));
        assert!(!move_project(&mut order, "a", "x"));
        assert!(!move_project(&mut order, "x", "a"));
        assert_eq!(names(&order), ["a", "b"]);
    }
}

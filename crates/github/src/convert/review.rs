//! Request bodies of the REST writes.

use crate::model::{DiffSide, ReviewSubmission, ReviewVerdict, Reviewer, ReviewerKind};
use serde_json::{Value, json};

fn side(side: DiffSide) -> &'static str {
    match side {
        DiffSide::Left => "LEFT",
        DiffSide::Right => "RIGHT",
    }
}

/// `POST pulls/N/reviews`: verdict, summary and every line comment in one
/// body. GitHub refuses a range that starts where it ends, so a one-line
/// range is sent as one line.
pub(crate) fn review_submission_body(review: &ReviewSubmission) -> Value {
    let event = match review.verdict {
        ReviewVerdict::Comment => "COMMENT",
        ReviewVerdict::Approve => "APPROVE",
        ReviewVerdict::RequestChanges => "REQUEST_CHANGES",
    };
    let comments: Vec<Value> = review
        .comments
        .iter()
        .map(|comment| {
            let mut body = json!({
                "path": comment.path,
                "line": comment.line,
                "side": side(comment.side),
                "body": comment.body,
            });
            if let Some(start_line) = comment.start_line {
                let start_side = comment.start_side.unwrap_or(comment.side);
                if start_line != comment.line || start_side != comment.side {
                    body["start_line"] = start_line.into();
                    body["start_side"] = side(start_side).into();
                }
            }
            body
        })
        .collect();
    json!({ "event": event, "body": review.body, "comments": comments })
}

/// `POST/DELETE pulls/N/requested_reviewers`: people and teams go in two
/// lists, both always present.
pub(crate) fn reviewer_request_body(reviewers: &[Reviewer]) -> Value {
    let of = |kind: ReviewerKind| -> Vec<&str> {
        reviewers
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| r.id.as_str())
            .collect()
    };
    json!({ "reviewers": of(ReviewerKind::User), "team_reviewers": of(ReviewerKind::Team) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReviewCommentDraft;

    fn draft(line: u64, side: DiffSide, start: Option<(u64, DiffSide)>) -> ReviewCommentDraft {
        ReviewCommentDraft {
            path: "src/b.ts".into(),
            line,
            side,
            start_line: start.map(|s| s.0),
            start_side: start.map(|s| s.1),
            body: "this line".into(),
        }
    }

    #[test]
    fn sends_the_verdict_the_summary_and_every_line_comment() {
        let body = review_submission_body(&ReviewSubmission {
            verdict: ReviewVerdict::RequestChanges,
            body: "Needs work".into(),
            comments: vec![
                draft(2, DiffSide::Right, None),
                draft(7, DiffSide::Left, None),
            ],
        });
        assert_eq!(
            body,
            json!({
                "event": "REQUEST_CHANGES",
                "body": "Needs work",
                "comments": [
                    {"path": "src/b.ts", "line": 2, "side": "RIGHT", "body": "this line"},
                    {"path": "src/b.ts", "line": 7, "side": "LEFT", "body": "this line"}
                ]
            })
        );
    }

    #[test]
    fn sends_a_range_from_its_first_line_and_a_one_line_range_as_one_line() {
        let body = review_submission_body(&ReviewSubmission {
            verdict: ReviewVerdict::Comment,
            body: String::new(),
            comments: vec![
                draft(9, DiffSide::Right, Some((4, DiffSide::Left))),
                draft(3, DiffSide::Right, Some((3, DiffSide::Right))),
            ],
        });
        assert_eq!(
            body["comments"],
            json!([
                {"path": "src/b.ts", "start_line": 4, "start_side": "LEFT", "line": 9, "side": "RIGHT", "body": "this line"},
                {"path": "src/b.ts", "line": 3, "side": "RIGHT", "body": "this line"}
            ])
        );
        assert_eq!(body["event"], "COMMENT");
    }

    #[test]
    fn sends_people_and_teams_in_their_own_lists_even_when_empty() {
        let body = reviewer_request_body(&[
            Reviewer {
                id: "octocat".into(),
                kind: ReviewerKind::User,
            },
            Reviewer {
                id: "core".into(),
                kind: ReviewerKind::Team,
            },
        ]);
        assert_eq!(
            body,
            json!({"reviewers": ["octocat"], "team_reviewers": ["core"]})
        );
        let only_people = reviewer_request_body(&[Reviewer {
            id: "a".into(),
            kind: ReviewerKind::User,
        }]);
        assert_eq!(
            only_people,
            json!({"reviewers": ["a"], "team_reviewers": []})
        );
    }
}

//! A waiting session's pending prompt, as a front-end draws it inline (a
//! Mission control tile's Approve / Deny, remote-control-bmej.5.6).
//!
//! Nothing here *detects* a prompt. Detection is FlightDeck Remote's — the
//! status edge, the agents' prompt sidecars and the session-file ingest in
//! [`crate::remote::bridge`] and [`crate::remote::transcript`] — and what it
//! produces is the same [`TranscriptItem::PermissionPrompt`] the phone renders.
//! This module only decides which of those prompts can be answered with one
//! click, and which answers to offer:
//!
//! - **Binary**: a permission prompt that has both an allow and a deny option.
//!   Approve / Deny.
//! - **Single-select**: a question with one single-select question of at most
//!   [`MAX_INLINE_OPTIONS`] options, on a backend that has an answer keystroke
//!   for every one of them. One button per option.
//! - **Unsupported**: anything else — a checklist, a multi-question
//!   `AskUserQuestion` form (a tabbed form whose keystrokes are out of scope
//!   here), too many options, or a custom agent whose keystrokes FlightDeck
//!   does not know. The front-end offers to open the session instead.
//!
//! An answer ([`PromptAnswer`]) is turned into the phone's own
//! `permission_decision` command ([`PromptAnswer::decision`]) so the host can
//! run it through the very translator the phone's answers take — the bytes a
//! click types are the bytes a phone tap types, by construction.

use flightdeck_remote_protocol::{
    CommandBody, PermissionChoice, PromptId, PromptKind, SessionId, TranscriptItem,
};

use crate::agents::setup::StatusBackend;
use crate::remote::commands::option_keystroke;
use crate::view::mission::SessionKey;

/// The most options a single-select prompt may have and still be answered
/// inline: more than this does not fit a tile's action row.
pub const MAX_INLINE_OPTIONS: usize = 4;

/// One answer a front-end can send back for a pending prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAnswer {
    /// Allow a binary permission prompt (once).
    Approve,
    /// Deny a binary permission prompt.
    Deny,
    /// Pick the option at this 0-based index of a single-select question.
    Option(u32),
}

impl PromptAnswer {
    /// The phone's `permission_decision` for this answer to `prompt_id` in
    /// `session_id`: `choice` for Approve / Deny, `option_index` for an
    /// option — exactly the fields the phone sets for the same tap, so the
    /// shared translator picks the same keystrokes.
    pub fn decision(self, session_id: SessionId, prompt_id: PromptId) -> CommandBody {
        let (choice, option_index) = match self {
            PromptAnswer::Approve => (Some(PermissionChoice::AllowOnce), None),
            PromptAnswer::Deny => (Some(PermissionChoice::Deny), None),
            PromptAnswer::Option(i) => (None, Some(i)),
        };
        CommandBody::PermissionDecision {
            session_id,
            prompt_id,
            choice,
            option_index,
            option_indices: None,
            free_text: None,
            answers: None,
        }
    }
}

/// A front-end's answer to one session's prompt: what
/// `HostEvent::AnswerPrompt` carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptReply {
    /// The session it answers.
    pub key: SessionKey,
    /// The prompt it answers ([`PromptView::prompt_id`]): a reply to a prompt
    /// that is no longer the pending one is refused.
    pub prompt_id: String,
    /// The answer.
    pub answer: PromptAnswer,
}

/// Which kind of inline answer a prompt allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptShape {
    /// Approve / Deny.
    Binary,
    /// One button per option.
    SingleSelect,
    /// No inline answer: open the session to answer it.
    Unsupported,
}

/// One button of a prompt's action row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptButton {
    /// What it says: the agent's own option label for a question, "Approve" /
    /// "Deny" for a permission.
    pub label: String,
    /// What clicking it answers.
    pub answer: PromptAnswer,
}

/// A session's pending prompt, as a tile draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptView {
    /// The prompt's id, echoed back with the answer so a click on a prompt
    /// that was answered or superseded meanwhile is refused, not typed into
    /// the next one.
    pub prompt_id: String,
    /// How it can be answered.
    pub shape: PromptShape,
    /// The question, or what the agent wants permission for (may be empty
    /// when the agent said nothing before asking).
    pub text: String,
    /// The buttons to offer, in order; empty for [`PromptShape::Unsupported`].
    pub buttons: Vec<PromptButton>,
}

/// The inline view of `item` for a session running `backend` (`None`: a custom
/// agent), or `None` when `item` is not a prompt.
pub fn prompt_view(item: &TranscriptItem, backend: Option<StatusBackend>) -> Option<PromptView> {
    let TranscriptItem::PermissionPrompt {
        prompt_id,
        kind,
        command,
        options,
        multi_select,
        questions,
        ..
    } = item
    else {
        return None;
    };
    let unsupported = |view: PromptView| PromptView {
        shape: PromptShape::Unsupported,
        buttons: Vec::new(),
        ..view
    };
    let mut view = PromptView {
        prompt_id: prompt_id.as_str().to_string(),
        shape: PromptShape::Unsupported,
        text: command.clone(),
        buttons: Vec::new(),
    };
    // The translator refuses a custom agent's prompts rather than guess at
    // its keys; so does the tile.
    let Some(backend) = backend else {
        return Some(view);
    };
    match kind {
        PromptKind::Permission => {
            let has = |c: PermissionChoice| options.iter().any(|o| o.choice == Some(c));
            if has(PermissionChoice::AllowOnce) && has(PermissionChoice::Deny) {
                view.shape = PromptShape::Binary;
                view.buttons = vec![
                    PromptButton {
                        label: "Approve".to_string(),
                        answer: PromptAnswer::Approve,
                    },
                    PromptButton {
                        label: "Deny".to_string(),
                        answer: PromptAnswer::Deny,
                    },
                ];
                Some(view)
            } else {
                Some(unsupported(view))
            }
        }
        PromptKind::Question => {
            // A multi-question form is tabs plus a Confirm tab; a checklist
            // toggles rather than submits. Neither is one keystroke.
            let single_question = questions.len() <= 1 && questions.iter().all(|q| !q.multi_select);
            let fits = !options.is_empty() && options.len() <= MAX_INLINE_OPTIONS;
            let keyable = options
                .iter()
                .all(|o| option_keystroke(backend, o.index).is_some());
            if single_question && !multi_select && fits && keyable {
                view.shape = PromptShape::SingleSelect;
                view.buttons = options
                    .iter()
                    .map(|o| PromptButton {
                        label: o.label.clone(),
                        answer: PromptAnswer::Option(o.index),
                    })
                    .collect();
                Some(view)
            } else {
                Some(unsupported(view))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flightdeck_remote_protocol::{ItemId, PermissionOption, PromptQuestion};

    fn option(index: u32, label: &str, choice: Option<PermissionChoice>) -> PermissionOption {
        PermissionOption {
            index,
            choice,
            label: label.to_string(),
            description: None,
        }
    }

    fn prompt(
        kind: PromptKind,
        options: Vec<PermissionOption>,
        multi_select: bool,
        questions: Vec<PromptQuestion>,
    ) -> TranscriptItem {
        TranscriptItem::PermissionPrompt {
            item_id: ItemId::new("i"),
            prompt_id: PromptId::new("s:p1"),
            kind,
            command: "Run ls?".to_string(),
            options,
            allow_free_text: false,
            multi_select,
            questions,
            at_ms: 0,
        }
    }

    fn binary() -> TranscriptItem {
        prompt(
            PromptKind::Permission,
            vec![
                option(0, "Allow once", Some(PermissionChoice::AllowOnce)),
                option(1, "Deny", Some(PermissionChoice::Deny)),
            ],
            false,
            Vec::new(),
        )
    }

    fn question(labels: &[&str]) -> TranscriptItem {
        prompt(
            PromptKind::Question,
            labels
                .iter()
                .enumerate()
                .map(|(i, l)| option(i as u32, l, None))
                .collect(),
            false,
            Vec::new(),
        )
    }

    #[test]
    fn a_permission_with_allow_and_deny_is_approve_deny_for_every_backend() {
        for backend in [
            StatusBackend::Claude,
            StatusBackend::Codex,
            StatusBackend::OpenCode,
        ] {
            let v = prompt_view(&binary(), Some(backend)).unwrap();
            assert_eq!(v.shape, PromptShape::Binary, "{backend:?}");
            assert_eq!(v.prompt_id, "s:p1");
            assert_eq!(v.text, "Run ls?");
            let answers: Vec<_> = v.buttons.iter().map(|b| b.answer).collect();
            assert_eq!(answers, [PromptAnswer::Approve, PromptAnswer::Deny]);
        }
    }

    #[test]
    fn a_permission_missing_a_side_is_unsupported() {
        let only_allow = prompt(
            PromptKind::Permission,
            vec![option(0, "Allow", Some(PermissionChoice::AllowOnce))],
            false,
            Vec::new(),
        );
        let v = prompt_view(&only_allow, Some(StatusBackend::OpenCode)).unwrap();
        assert_eq!(v.shape, PromptShape::Unsupported);
        assert!(v.buttons.is_empty());
    }

    #[test]
    fn a_short_single_select_question_offers_its_options() {
        for backend in [StatusBackend::Claude, StatusBackend::OpenCode] {
            let v = prompt_view(&question(&["Pizza", "Sushi", "Tacos"]), Some(backend)).unwrap();
            assert_eq!(v.shape, PromptShape::SingleSelect, "{backend:?}");
            let labels: Vec<_> = v.buttons.iter().map(|b| b.label.as_str()).collect();
            assert_eq!(labels, ["Pizza", "Sushi", "Tacos"]);
            assert_eq!(v.buttons[2].answer, PromptAnswer::Option(2));
        }
    }

    #[test]
    fn unsupported_shapes_offer_no_buttons() {
        let checklist = prompt(
            PromptKind::Question,
            vec![option(0, "Tests", None), option(1, "Clippy", None)],
            true,
            Vec::new(),
        );
        let q = |text: &str| PromptQuestion {
            header: None,
            question: text.to_string(),
            options: vec![option(0, "A", None), option(1, "B", None)],
            multi_select: false,
        };
        let multi_question = prompt(
            PromptKind::Question,
            vec![option(0, "A", None), option(1, "B", None)],
            false,
            vec![q("First?"), q("Second?")],
        );
        let too_many = question(&["1", "2", "3", "4", "5"]);
        let cases = [
            ("checklist", checklist, Some(StatusBackend::Claude)),
            (
                "multi-question",
                multi_question,
                Some(StatusBackend::Claude),
            ),
            ("too many options", too_many, Some(StatusBackend::OpenCode)),
            // Codex has no question keystrokes at all.
            (
                "codex question",
                question(&["A", "B"]),
                Some(StatusBackend::Codex),
            ),
            ("custom agent", binary(), None),
        ];
        for (what, item, backend) in cases {
            let v = prompt_view(&item, backend).unwrap();
            assert_eq!(v.shape, PromptShape::Unsupported, "{what}");
            assert!(v.buttons.is_empty(), "{what}");
        }
    }

    #[test]
    fn a_non_prompt_item_has_no_view() {
        let item = TranscriptItem::UserMessage {
            item_id: ItemId::new("u"),
            text: "hi".to_string(),
            at_ms: 0,
        };
        assert_eq!(prompt_view(&item, Some(StatusBackend::Claude)), None);
    }

    #[test]
    fn answers_become_the_phones_decision_fields() {
        let sid = || SessionId::new("s");
        let pid = || PromptId::new("s:p1");
        let fields = |body: CommandBody| match body {
            CommandBody::PermissionDecision {
                choice,
                option_index,
                option_indices,
                free_text,
                answers,
                ..
            } => {
                assert!(option_indices.is_none() && free_text.is_none() && answers.is_none());
                (choice, option_index)
            }
            other => panic!("not a decision: {other:?}"),
        };
        assert_eq!(
            fields(PromptAnswer::Approve.decision(sid(), pid())),
            (Some(PermissionChoice::AllowOnce), None)
        );
        assert_eq!(
            fields(PromptAnswer::Deny.decision(sid(), pid())),
            (Some(PermissionChoice::Deny), None)
        );
        assert_eq!(
            fields(PromptAnswer::Option(3).decision(sid(), pid())),
            (None, Some(3))
        );
    }
}

//! How what an extension's supervisor, the hub or the extension answered is said to a person: in the application's own
//! words, never the extension's.
use super::*;
/// What a user is told about a plugin, from what its supervisor last reported.
///
/// `None` means there is no supervisor entry at all, because the plugin could not be
/// launched. Nothing then says it is running, so it is reported as stopped: a screen must
/// never claim a plugin works on the strength of the absence of bad news.
pub(crate) fn status_of(event: Option<&PluginEvent>) -> peersus_core::model::PluginStatus {
    use peersus_core::model::PluginStatus;
    match event {
        // Launched is not usable: until the process has said hello the hub cannot reach it,
        // so it is only starting, however quickly that usually happens.
        Some(PluginEvent::Started) => PluginStatus::Starting,
        Some(PluginEvent::Connected) => PluginStatus::Running,
        Some(PluginEvent::Failed { .. }) => PluginStatus::Restarting,
        Some(PluginEvent::GaveUp) => PluginStatus::Failed,
        Some(PluginEvent::Stopped) | None => PluginStatus::Stopped,
    }
}

/// The report a supervisor has made since this was last asked, if there is one.
///
/// The obvious way to ask is `has_changed`, and it is wrong exactly when it matters: it
/// fails as soon as the supervisor has ended, before it looks at whether there was
/// something unseen. A supervisor that gives up sends `GaveUp` and ends in the same
/// instant, so that call would hide the one report a user most needs. `changed` looks at
/// whether there is something unseen first, and only then at whether the sender is gone,
/// so an unseen last report is delivered once and a finished supervisor then adds nothing.
///
/// It is polled a single time with a waker that does nothing: the answer is already
/// there or it is "nothing new", and it must not wait.
pub(crate) fn observe(health: &mut watch::Receiver<PluginEvent>) -> Option<PluginEvent> {
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    let mut context = Context::from_waker(Waker::noop());
    // The future borrows the receiver, so it is dropped before the value is read.
    let arrived = {
        let mut changed = pin!(health.changed());
        matches!(changed.as_mut().poll(&mut context), Poll::Ready(Ok(())))
    };
    arrived.then(|| health.borrow_and_update().clone())
}

/// What the user is told when the hub refuses to pass on an action. Every refusal the hub
/// can give has its own fixed wording, so a person is never sent looking for a crash that
/// did not happen: being refused is not the same as the plugin being gone.
pub(crate) fn refusal_text(error: &peersus_api::Error) -> &'static str {
    use peersus_api::Error;
    match error {
        Error::Denied => "The plugin does not allow this action",
        Error::QueueFull => "Too many operations are pending",
        Error::RateLimited => "The plugin is sending too much, too fast",
        Error::UnknownProvider | Error::Unavailable => "The plugin is not running",
        Error::UnknownMethod | Error::Version => "The plugin does not offer this action",
        _ => "The request could not be sent",
    }
}

/// The plugin a command is about, when it is about one: what a refusal of it is said of.
pub(super) fn command_plugin(command: &Command) -> Option<PluginId> {
    match command {
        Command::ForwardInput { plugin, .. }
        | Command::PageEvent { plugin, .. }
        | Command::InvokeAction { plugin, .. }
        | Command::InvokeActionWithContext { plugin, .. }
        | Command::InvokeSelectedPathAction { plugin, .. }
        | Command::InvokeSelectedAction { plugin, .. }
        | Command::InvokeSharedText { plugin, .. }
        | Command::SetPluginSetting { plugin, .. } => Some(plugin.clone()),
        _ => None,
    }
}

/// Where an action stands, given what came back for its call.
///
/// Only the code is read, and only to match it against the codes this application and the
/// hub themselves use, exactly and in full. The message is never looked at, and a code that
/// is not one of those (anything a plugin made up, in any spelling) is `Other`, so nothing a
/// plugin wrote can reach what a person is shown.
pub(crate) fn action_state(
    result: &peersus_api::message::Outcome,
) -> peersus_core::model::ActionState {
    use peersus_api::message::Outcome;
    use peersus_core::model::{ActionFailure, ActionState};
    match result {
        Outcome::Ok { .. } => ActionState::Done,
        Outcome::Error { code, .. } if code == "cancelled" => ActionState::Cancelled,
        Outcome::Error { code, .. } => ActionState::Failed {
            reason: match code.as_str() {
                "denied" | "refused" => ActionFailure::NotAllowed,
                "unavailable" => ActionFailure::Unavailable,
                "stopped" => ActionFailure::Stopped,
                "timeout" | "invalid-deadline" => ActionFailure::TimedOut,
                "busy" => ActionFailure::Busy,
                _ => ActionFailure::Other,
            },
        },
    }
}

/// The fixed sentence a person is told when a plugin's call ended in an error, by the reason `action_state` read from its
/// code. Only the application's own words: what the plugin wrote is never in it. `None` for an action that was cancelled
/// or that did not fail (nothing to tell).
pub(crate) fn action_failure_sentence(
    result: &peersus_api::message::Outcome,
) -> Option<&'static str> {
    use peersus_core::model::{ActionFailure, ActionState};
    match action_state(result) {
        ActionState::Failed { reason } => Some(match reason {
            ActionFailure::NotAllowed => "A plugin was not allowed to complete the action",
            ActionFailure::Unavailable => {
                "A plugin could not complete the action: what it needs is not available"
            }
            ActionFailure::Stopped => "A plugin stopped before it finished the action",
            ActionFailure::TimedOut => "A plugin did not complete the action in time",
            ActionFailure::Busy => "A plugin could not complete the action: it is busy",
            _ => "A plugin could not complete the action",
        }),
        _ => None,
    }
}

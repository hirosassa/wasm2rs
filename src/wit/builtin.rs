//! The canonical built-ins of a component (`canon task.return`, `context.get`,
//! `waitable-set.*`, `stream.*`, `future.*`, ...), classified by the feature they belong to.
//!
//! WASI 0.3 guests import these as core functions that no WIT interface lowers. The glue
//! cannot implement them without the async runtime (WASI 0.3 M2), so the reader only
//! identifies them: [`Builtin::unsupported_feature`] names the feature that is missing, so
//! the error says what a guest needs instead of "unknown import".

use wasmparser::CanonicalFunction;

/// A canonical built-in other than `canon lift` and `canon lower`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Builtin {
    /// `resource.new`, `resource.drop` and `resource.rep`: handled by the resource glue.
    Resource,
    /// `task.return` and `task.cancel`: completion of an async export.
    Task,
    /// `context.get` and `context.set`: the per-task context slots.
    Context,
    /// `waitable-set.*` and `waitable.join`.
    Waitable,
    /// `subtask.drop` and `subtask.cancel`: a call of an async-lowered import.
    Subtask,
    /// `stream.*`.
    Stream,
    /// `future.*`.
    Future,
    /// `error-context.*`.
    ErrorContext,
    /// `thread.*`, `backpressure.*` and `thread.yield`: the cooperative-threads and
    /// scheduling built-ins.
    Scheduling,
}

impl Builtin {
    /// The built-in a `canon` definition is, or `None` for `canon lift` and `canon lower`.
    pub fn of(func: &CanonicalFunction) -> Option<Self> {
        use CanonicalFunction as C;
        Some(match func {
            C::Lift { .. } | C::Lower { .. } => return None,
            C::ResourceNew { .. } | C::ResourceDrop { .. } | C::ResourceRep { .. } => {
                Self::Resource
            }
            C::TaskReturn { .. } | C::TaskCancel => Self::Task,
            C::ContextGet { .. } | C::ContextSet { .. } => Self::Context,
            C::WaitableSetNew
            | C::WaitableSetWait { .. }
            | C::WaitableSetPoll { .. }
            | C::WaitableSetDrop
            | C::WaitableJoin => Self::Waitable,
            C::SubtaskDrop | C::SubtaskCancel { .. } => Self::Subtask,
            C::StreamNew { .. }
            | C::StreamRead { .. }
            | C::StreamWrite { .. }
            | C::StreamForward { .. }
            | C::StreamCancelRead { .. }
            | C::StreamCancelWrite { .. }
            | C::StreamDropReadable { .. }
            | C::StreamDropWritable { .. } => Self::Stream,
            C::FutureNew { .. }
            | C::FutureRead { .. }
            | C::FutureWrite { .. }
            | C::FutureForward { .. }
            | C::FutureCancelRead { .. }
            | C::FutureCancelWrite { .. }
            | C::FutureDropReadable { .. }
            | C::FutureDropWritable { .. } => Self::Future,
            C::ErrorContextNew { .. }
            | C::ErrorContextDebugMessage { .. }
            | C::ErrorContextDrop => Self::ErrorContext,
            // The remaining ones are the thread, backpressure and yield built-ins.
            _ => Self::Scheduling,
        })
    }

    /// What the guest needs from the runtime when it imports this built-in, or `None` when the
    /// glue already serves it.
    pub fn unsupported_feature(self) -> Option<&'static str> {
        match self {
            Self::Resource => None,
            Self::Task => Some("`task.return` / `task.cancel` (completing an async export)"),
            Self::Context => Some("`context.get` / `context.set` (per-task context slots)"),
            Self::Waitable => Some("`waitable-set.*` / `waitable.join` (waiting for events)"),
            Self::Subtask => Some("`subtask.drop` / `subtask.cancel` (async-lowered import calls)"),
            Self::Stream => Some("`stream.*` (stream reads and writes)"),
            Self::Future => Some("`future.*` (future reads and writes)"),
            Self::ErrorContext => Some("`error-context.*`"),
            Self::Scheduling => Some("`thread.*` / `backpressure.*` / `thread.yield`"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lift_or_lower_is_not_a_builtin() {
        let lower = CanonicalFunction::Lower {
            func_index: 0,
            options: Box::new([]),
        };
        assert_eq!(Builtin::of(&lower), None);
    }

    #[test]
    fn resource_builtins_are_served_and_the_async_ones_are_named() {
        let drop = CanonicalFunction::ResourceDrop { resource: 0 };
        assert_eq!(Builtin::of(&drop), Some(Builtin::Resource));
        assert_eq!(Builtin::Resource.unsupported_feature(), None);
        for (func, builtin, needle) in [
            (CanonicalFunction::TaskCancel, Builtin::Task, "task.return"),
            (
                CanonicalFunction::WaitableSetNew,
                Builtin::Waitable,
                "waitable-set",
            ),
            (CanonicalFunction::SubtaskDrop, Builtin::Subtask, "subtask"),
            (
                CanonicalFunction::ThreadYield,
                Builtin::Scheduling,
                "thread.yield",
            ),
        ] {
            assert_eq!(Builtin::of(&func), Some(builtin));
            assert!(builtin.unsupported_feature().unwrap().contains(needle));
        }
    }
}

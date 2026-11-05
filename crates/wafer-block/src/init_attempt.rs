//! What one init attempt's database calls saw.
//!
//! A block's `lifecycle(Init)` can fail because its database could not
//! answer, and the block may report that any way it likes: propagate the
//! database's error, wrap it in a new one, or hang until the init budget cuts
//! it off. So the runtime does not learn it from the error the block returns.
//! It keeps an [`InitAttemptRecord`] per attempt, reachable from the attempt's
//! context and every context derived from it
//! ([`Context::init_attempt_record`](crate::context::Context::init_attempt_record)),
//! and the database handler — where the database's typed answer is known —
//! notes each call into it: when a call starts and ends, and how a failed one
//! failed ([`DatabaseAnswer`]). When the attempt settles, the runtime reads
//! what it saw and that, when there is anything, is the failure's detail code.
//!
//! An attempt started from inside another (block A's `Init` reaching block
//! B's) has the outer attempt's record as its parent: what B's calls see is
//! also what A's attempt saw. A record is closed when its attempt settles;
//! a call made through a context kept past that (deferred work) notes
//! nothing, so one attempt never reads another's.

use std::sync::{Arc, Mutex, PoisonError};

use crate::wire::database::{CALL_BUDGET_EXHAUSTED, DATABASE_CONTENDED, DATABASE_UNAVAILABLE};

/// How a database call failed, for an [`InitAttemptRecord`]: the classes the
/// database's typed error tells apart that say something about the database
/// rather than the request. Ordered by precedence: an attempt that saw more
/// than one reports the greatest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DatabaseAnswer {
    /// The database was up but busy with other work (a lock, a deadlock or
    /// serialization conflict, no connection of the application's own free).
    Contended,
    /// The invocation's database call budget was spent.
    CallBudgetExhausted,
    /// The database could not answer: down, unreachable, restarting, reset,
    /// over a quota that resets — or a call still waiting on it when the
    /// init budget ran out.
    Unavailable,
}

impl DatabaseAnswer {
    /// The detail code ([`crate::WaferError::detail_code`]) of this class.
    #[must_use]
    pub const fn detail_code(self) -> &'static str {
        match self {
            Self::Contended => DATABASE_CONTENDED,
            Self::CallBudgetExhausted => CALL_BUDGET_EXHAUSTED,
            Self::Unavailable => DATABASE_UNAVAILABLE,
        }
    }
}

#[derive(Debug, Default)]
struct RecordState {
    /// The greatest answer a failed call got, if any.
    worst: Option<DatabaseAnswer>,
    /// Calls started and not yet answered.
    in_flight: usize,
    /// The attempt has settled: nothing more is noted.
    closed: bool,
}

/// What one init attempt's database calls saw (see the module doc).
#[derive(Debug, Default)]
pub struct InitAttemptRecord {
    parent: Option<Arc<InitAttemptRecord>>,
    state: Mutex<RecordState>,
}

impl InitAttemptRecord {
    /// A record for a new attempt, inside `parent`'s if it is started from
    /// one.
    #[must_use]
    pub fn new(parent: Option<Arc<Self>>) -> Arc<Self> {
        Arc::new(Self {
            parent,
            state: Mutex::default(),
        })
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut RecordState) -> T) -> T {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// This record and every attempt it runs inside.
    fn lineage(&self) -> impl Iterator<Item = &Self> {
        std::iter::successors(Some(self), |record| record.parent.as_deref())
    }

    /// A database call starts; it is answered when the returned guard drops.
    /// A call still unanswered when the attempt's init budget runs out counts
    /// as [`DatabaseAnswer::Unavailable`] ([`Self::note_unanswered`]).
    #[must_use]
    pub fn call(self: &Arc<Self>) -> DatabaseCall {
        for record in self.lineage() {
            record.with_state(|state| {
                if !state.closed {
                    state.in_flight += 1;
                }
            });
        }
        DatabaseCall {
            record: self.clone(),
        }
    }

    /// A database call failed with `answer`. Noted in this record and every
    /// attempt it runs inside, unless this attempt has settled.
    pub fn note(&self, answer: DatabaseAnswer) {
        if self.with_state(|state| state.closed) {
            return;
        }
        for record in self.lineage() {
            record.with_state(|state| {
                if !state.closed {
                    state.worst = state.worst.max(Some(answer));
                }
            });
        }
    }

    /// The attempt's init budget ran out: a call still waiting on the
    /// database is the database not answering.
    pub fn note_unanswered(&self) {
        if self.with_state(|state| state.in_flight > 0) {
            self.note(DatabaseAnswer::Unavailable);
        }
    }

    /// The attempt settles: what its calls saw, and nothing is noted after.
    pub fn close(&self) -> Option<DatabaseAnswer> {
        self.with_state(|state| {
            state.closed = true;
            state.worst
        })
    }
}

/// One database call of an attempt, answered when dropped
/// ([`InitAttemptRecord::call`]).
#[derive(Debug)]
pub struct DatabaseCall {
    record: Arc<InitAttemptRecord>,
}

impl Drop for DatabaseCall {
    fn drop(&mut self) {
        for record in self.record.lineage() {
            record.with_state(|state| state.in_flight = state.in_flight.saturating_sub(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_greatest_answer_wins_and_reaches_every_outer_attempt() {
        let outer = InitAttemptRecord::new(None);
        let inner = InitAttemptRecord::new(Some(outer.clone()));
        inner.note(DatabaseAnswer::Contended);
        inner.note(DatabaseAnswer::Unavailable);
        inner.note(DatabaseAnswer::CallBudgetExhausted);
        assert_eq!(inner.close(), Some(DatabaseAnswer::Unavailable));
        assert_eq!(outer.close(), Some(DatabaseAnswer::Unavailable));
    }

    #[test]
    fn a_settled_attempt_notes_nothing_more() {
        let outer = InitAttemptRecord::new(None);
        let inner = InitAttemptRecord::new(Some(outer.clone()));
        assert_eq!(inner.close(), None);
        inner.note(DatabaseAnswer::Unavailable);
        let _late = inner.call();
        inner.note_unanswered();
        assert_eq!(outer.close(), None, "a settled inner attempt's late call");
        let fresh = InitAttemptRecord::new(None);
        assert_eq!(fresh.close(), None, "another attempt's record is its own");
    }

    #[test]
    fn a_call_unanswered_at_the_budget_is_unavailable() {
        let record = InitAttemptRecord::new(None);
        {
            let _answered = record.call();
        }
        record.note_unanswered();
        let _hung = record.call();
        let outer_free = record.clone();
        outer_free.note_unanswered();
        assert_eq!(record.close(), Some(DatabaseAnswer::Unavailable));
        let quiet = InitAttemptRecord::new(None);
        quiet.note_unanswered();
        assert_eq!(quiet.close(), None, "no call in flight");
    }
}

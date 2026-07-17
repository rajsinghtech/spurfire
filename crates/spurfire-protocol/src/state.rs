//! Lobby lifecycle state machine.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Every externally observable lobby lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LobbyState {
    /// Capability checks or record-level provisioning are in progress.
    Provisioning,
    /// The lobby accepts joins and connectivity measurements.
    Forming,
    /// The roster has fresh measurements and an authority can be elected.
    Ready,
    /// The map seed is fixed and the roster is frozen pending heartbeat.
    Starting,
    /// Peer-hosted gameplay is active.
    InMatch,
    /// Results were accepted or destruction was requested; teardown is running.
    Closing,
    /// A terminal failure. A machine-readable state reason is mandatory.
    Failed,
    /// A terminal idle- or absolute-TTL expiry.
    Expired,
    /// Teardown has been attempted for all known resources.
    Destroyed,
}

impl LobbyState {
    /// Stable list of all states, useful for exhaustive validation and clients.
    pub const ALL: [Self; 9] = [
        Self::Provisioning,
        Self::Forming,
        Self::Ready,
        Self::Starting,
        Self::InMatch,
        Self::Closing,
        Self::Failed,
        Self::Expired,
        Self::Destroyed,
    ];

    /// Returns the exact legal successors specified by the lobby contract.
    #[must_use]
    pub const fn legal_successors(self) -> &'static [Self] {
        match self {
            Self::Provisioning => &[Self::Forming, Self::Failed],
            Self::Forming => &[Self::Ready, Self::Closing, Self::Expired],
            Self::Ready => &[Self::Starting, Self::Forming, Self::Closing, Self::Expired],
            Self::Starting => &[Self::InMatch, Self::Failed],
            Self::InMatch => &[Self::Closing, Self::Failed],
            Self::Closing => &[Self::Destroyed],
            Self::Failed | Self::Expired | Self::Destroyed => &[],
        }
    }

    /// Returns whether `next` is a real legal state transition.
    ///
    /// A same-state idempotent API replay is a no-op, not a transition, and
    /// therefore returns `false` here.
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        self.legal_successors().contains(&next)
    }

    /// Validates one state transition.
    pub fn validate_transition(self, next: Self) -> Result<(), LobbyTransitionError> {
        if self.can_transition_to(next) {
            Ok(())
        } else {
            Err(LobbyTransitionError {
                from: self,
                to: next,
            })
        }
    }

    /// Returns whether no future state mutation is legal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(Self::legal_successors(self), [])
    }

    /// Returns whether connectivity reports are accepted in this state.
    #[must_use]
    pub const fn accepts_measurements(self) -> bool {
        matches!(self, Self::Forming | Self::Ready | Self::InMatch)
    }

    /// Returns whether a machine-readable `state_reason` is mandatory.
    #[must_use]
    pub const fn requires_state_reason(self) -> bool {
        matches!(self, Self::Failed)
    }
}

/// An attempted transition absent from [`LobbyState::legal_successors`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("illegal lobby transition from {from:?} to {to:?}")]
pub struct LobbyTransitionError {
    /// Existing state.
    pub from: LobbyState,
    /// Requested state.
    pub to: LobbyState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_matrix_is_exhaustive() {
        let legal = [
            (LobbyState::Provisioning, LobbyState::Forming),
            (LobbyState::Provisioning, LobbyState::Failed),
            (LobbyState::Forming, LobbyState::Ready),
            (LobbyState::Forming, LobbyState::Closing),
            (LobbyState::Forming, LobbyState::Expired),
            (LobbyState::Ready, LobbyState::Starting),
            (LobbyState::Ready, LobbyState::Forming),
            (LobbyState::Ready, LobbyState::Closing),
            (LobbyState::Ready, LobbyState::Expired),
            (LobbyState::Starting, LobbyState::InMatch),
            (LobbyState::Starting, LobbyState::Failed),
            (LobbyState::InMatch, LobbyState::Closing),
            (LobbyState::InMatch, LobbyState::Failed),
            (LobbyState::Closing, LobbyState::Destroyed),
        ];

        for from in LobbyState::ALL {
            for to in LobbyState::ALL {
                let expected = legal.contains(&(from, to));
                assert_eq!(
                    from.can_transition_to(to),
                    expected,
                    "unexpected {from:?} -> {to:?} result"
                );
                assert_eq!(from.validate_transition(to).is_ok(), expected);
            }
        }
    }

    #[test]
    fn failed_expired_and_destroyed_are_terminal() {
        assert!(LobbyState::Failed.is_terminal());
        assert!(LobbyState::Expired.is_terminal());
        assert!(LobbyState::Destroyed.is_terminal());
        assert!(!LobbyState::Closing.is_terminal());
    }
}

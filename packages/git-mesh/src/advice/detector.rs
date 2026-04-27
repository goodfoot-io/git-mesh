//! `Detector` trait — the seam between `CandidateInput` and `Vec<Suggestion>`.

use crate::advice::candidates::CandidateInput;
use crate::advice::suggestion::Suggestion;

/// A stateless detector that mines n-ary mesh suggestions from a
/// `CandidateInput`. Implementations are pure: given the same input
/// they produce the same output with no side effects.
pub trait Detector {
    /// Run the detector against `input` and return any suggestions.
    fn detect(&self, input: &CandidateInput<'_>) -> Vec<Suggestion>;
}

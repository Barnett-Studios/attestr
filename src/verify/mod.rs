pub mod behavioral;
pub mod standing;
pub mod structural;

pub use baseplate::patterns;

// The types the verifiers take and return. Aliases for convenience next to the functions
// that use them; `attestr::model` is the complete re-export (attestr#22).
pub use baseplate::model::{
    Confidence, Method, MethodOutcome, Observation, PromiseSpec, VerificationResult,
};

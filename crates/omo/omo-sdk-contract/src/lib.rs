//! The omo SDK contract (oh-my-openagent `packages/omo-senpi/src/components/omo-sdk/contract`, v5.1.84): the security
//! design's principals (#14945, section 3), the principal-by-capability matrix of section 4 with its refusals, the
//! approved behaviour changes of section 10, the failure-code registry of design 4.4, the closed answers and
//! inputs of the credential-bearing calls, `account.adopt` among them, and the root's typed call table as data. A call
//! here is a type, never authority: the matrix says who may make it, and the core enforces it.

pub mod behaviour_changes;
pub mod calls;
pub mod code_sources;
pub mod codes;
pub mod inputs;
pub mod namespaces;
pub mod principal;
pub mod principal_matrix;
pub mod results;
pub mod schema;
pub mod shape;

pub use behaviour_changes::{BEHAVIOUR_CHANGES, BehaviourChange, ChangeId, asserted_expectation};
pub use calls::{
    AnswerShape, CallSpec, ROOT_CALLS, answer_shape, call, calls_of, can_wait, input_accepts,
    input_omittable, is_root_call, namespace_of, takes_deadline_option,
};
pub use code_sources::OMO_SDK_CODES;
pub use codes::{CodeEntry, HELD_CODES, HeldCode, OMO_CODE_REGISTRY};
pub use inputs::custody::custody_input_accepts;
pub use principal::{PRINCIPAL_KINDS, PrincipalKind, is_principal};
pub use principal_matrix::{Cell, MatrixLine, Refusal, principal_matrix, refusals};
pub use results::auth_check::{
    AUTH_ACCOUNT_CHECK_CODES, AUTH_CHECK_FREE_FIELDS, FreeField, FreeFieldOrigin,
};
pub use results::custody::{
    CUSTODY_RESULT_CALLS, SESSION_RESULT_CALLS, custody_answer_accepts, session_answer_accepts,
};
pub use results::root::{answer_accepts, answer_spec};

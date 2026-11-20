//! Library surface of the `rlt` CLI: the reusable IR-matcher oracle scoring, shared by the `rlt`
//! binary (`score-oracle`) and the differential-oracle integration test, and the comparison with
//! LanguageTool behind `cargo xtask coverage`.

pub mod coverage;
pub mod oracle_score;

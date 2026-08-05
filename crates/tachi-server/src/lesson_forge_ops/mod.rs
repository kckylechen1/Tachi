// Compatibility shim for old crate::lesson_forge_ops::* paths.
#[allow(unused_imports)]
pub(crate) use tachi_lesson_forge::{
    discrimination, forge, pilot, privacy, progress, report, runner, selection, source,
};

pub(crate) mod storage {
    pub(crate) use tachi_lesson_forge::LESSON_CANDIDATE_DOMAIN;
}

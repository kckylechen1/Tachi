use std::cell::RefCell;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StatusIoHookStage {
    AfterDirectoryValidation,
    BeforeAtomicRename,
}

type StatusIoHook = (StatusIoHookStage, PathBuf, Box<dyn FnOnce(&Path)>);

thread_local! {
    static STATUS_IO_HOOK: RefCell<Option<StatusIoHook>> = RefCell::new(None);
}

pub(super) fn install_status_io_hook(
    stage: StatusIoHookStage,
    target: PathBuf,
    hook: impl FnOnce(&Path) + 'static,
) {
    STATUS_IO_HOOK.with(|slot| {
        let previous = slot.replace(Some((stage, target, Box::new(hook))));
        assert!(
            previous.is_none(),
            "managed status I/O hook already installed"
        );
    });
}

pub(super) fn run_status_io_hook(stage: StatusIoHookStage, target: &Path) {
    let hook = STATUS_IO_HOOK.with(|slot| {
        let matches = slot
            .borrow()
            .as_ref()
            .is_some_and(|(expected_stage, expected_target, _)| {
                *expected_stage == stage && expected_target == target
            });
        matches.then(|| slot.borrow_mut().take().expect("hook exists").2)
    });
    if let Some(hook) = hook {
        hook(target);
    }
}

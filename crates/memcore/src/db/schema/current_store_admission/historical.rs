//! Explicit frozen reader for the D7 E39 -> synthetic E40/E41 regression suite.
//! Ordinary catalogue overrides never change the shipped inventory cache.

use super::*;
use std::cell::Cell;

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
}
static V39_PORTABLE: OnceLock<RequiredInventory> = OnceLock::new();

pub(super) fn enabled() -> bool {
    ENABLED.with(Cell::get)
}

pub(crate) fn with_historical_d7<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            ENABLED.with(|flag| flag.set(self.0));
        }
    }
    let _restore = Restore(ENABLED.with(|flag| flag.replace(true)));
    crate::db::migrations::catalogue::test_support::with_prefix(39, run)
}

pub(super) fn inventory() -> Result<&'static RequiredInventory, MemoryError> {
    if let Some(inventory) = V39_PORTABLE.get() {
        return Ok(inventory);
    }
    let built = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                crate::db::migrations::catalogue::test_support::with_prefix(39, || {
                    build_required_inventory(StoreProfile::PortableKernel, 39)
                })
            })
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })?;
    let _ = V39_PORTABLE.set(built);
    Ok(V39_PORTABLE
        .get()
        .expect("complete historical v39 inventory"))
}

#[test]
fn historical_scope_and_plain_catalogue_overrides_cannot_poison_shipped_inventory() {
    let has_typed = || {
        required_inventory(StoreProfile::PortableKernel)
            .unwrap()
            .required
            .contains_key("table:typed_records")
    };
    with_historical_d7(|| {
        assert!(!has_typed());
        with_historical_d7(|| assert!(!has_typed()));
        assert!(!has_typed());
    });
    assert!(has_typed());
    crate::db::migrations::catalogue::test_support::with_prefix(39, || assert!(has_typed()));
    assert!(std::panic::catch_unwind(|| with_historical_d7(|| panic!("restore scope"))).is_err());
    assert!(has_typed());
}

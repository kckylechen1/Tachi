//! Frozen Portable inventories for D7 and the v41–43 compatibility band.
//! Ordinary catalogue overrides never change the shipped inventory cache.

use super::*;
use std::cell::Cell;

thread_local! {
    static VERSION: Cell<Option<u32>> = const { Cell::new(None) };
}
static V39_PORTABLE: OnceLock<RequiredInventory> = OnceLock::new();
static V43_PORTABLE: OnceLock<RequiredInventory> = OnceLock::new();

pub(super) fn version() -> Option<u32> {
    VERSION.with(Cell::get)
}

pub(crate) fn with_historical_d7<T>(run: impl FnOnce() -> T) -> T {
    with_historical_portable_inventory(39, run)
}

pub(crate) fn with_historical_portable_inventory<T>(version: u32, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<u32>);
    impl Drop for Restore {
        fn drop(&mut self) {
            VERSION.with(|flag| flag.set(self.0));
        }
    }
    let _restore = Restore(VERSION.with(|flag| flag.replace(Some(version))));
    crate::db::migrations::catalogue::test_support::with_prefix(version, run)
}

pub(super) fn inventory(version: u32) -> Result<&'static RequiredInventory, MemoryError> {
    let cache = match version {
        39 => &V39_PORTABLE,
        43 => &V43_PORTABLE,
        _ => unreachable!("unsupported historical inventory"),
    };
    if let Some(inventory) = cache.get() {
        return Ok(inventory);
    }
    let built = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                crate::db::migrations::catalogue::test_support::with_prefix(version, || {
                    build_required_inventory(StoreProfile::PortableKernel, version)
                })
            })
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })?;
    let _ = cache.set(built);
    Ok(cache.get().expect("complete historical inventory"))
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

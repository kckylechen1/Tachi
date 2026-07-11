use portable_kernel::{ADMIN_SURFACE_ENABLED, IS_PORTABLE_BUILD};

#[test]
fn portable_build_disables_admin_surface() {
    assert!(
        !ADMIN_SURFACE_ENABLED,
        "portable-kernel must resolve memcore without the product admin surface"
    );
    assert!(
        IS_PORTABLE_BUILD,
        "portable-kernel must report its portable build"
    );
}

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_read_pool_allows_concurrent_read_closures() {
    let server = make_server();
    assert!(
        server.global_read_pool_size_for_tests() >= 2,
        "test requires the default read pool to have multiple slots"
    );

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let first = {
        let server = server.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        let entered = std::sync::Arc::clone(&entered);
        tokio::task::spawn_blocking(move || {
            server.with_global_store_read(|_store| {
                entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                barrier.wait();
                Ok(())
            })
        })
    };
    let second = {
        let server = server.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        let entered = std::sync::Arc::clone(&entered);
        tokio::task::spawn_blocking(move || {
            server.with_global_store_read(|_store| {
                entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                barrier.wait();
                Ok(())
            })
        })
    };

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        first
            .await
            .expect("first reader task should join")
            .expect("first reader should succeed");
        second
            .await
            .expect("second reader task should join")
            .expect("second reader should succeed");
    })
    .await
    .expect("two readers should enter read closures concurrently");

    assert_eq!(
        entered.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "both reader closures should have entered before either returned"
    );
}

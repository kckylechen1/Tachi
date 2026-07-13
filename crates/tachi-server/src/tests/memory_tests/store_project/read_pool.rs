use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_read_pool_allows_concurrent_read_closures() {
    let server = make_server();
    assert!(
        server.global_read_pool_size_for_tests() >= 2,
        "test requires the default read pool to have multiple slots"
    );

    let release = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();

    let first = {
        let server = server.clone();
        let release = std::sync::Arc::clone(&release);
        let entered = std::sync::Arc::clone(&entered);
        let entered_tx = entered_tx.clone();
        tokio::task::spawn_blocking(move || {
            server.with_global_store_read(|_store| {
                entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                entered_tx
                    .send(())
                    .expect("reader entrance receiver should remain open");
                let (released, wake) = &*release;
                let mut released = released.lock().expect("lock reader release gate");
                while !*released {
                    released = wake.wait(released).expect("wait for reader release gate");
                }
                Ok(())
            })
        })
    };
    let second = {
        let server = server.clone();
        let release = std::sync::Arc::clone(&release);
        let entered = std::sync::Arc::clone(&entered);
        let entered_tx = entered_tx.clone();
        tokio::task::spawn_blocking(move || {
            server.with_global_store_read(|_store| {
                entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                entered_tx
                    .send(())
                    .expect("reader entrance receiver should remain open");
                let (released, wake) = &*release;
                let mut released = released.lock().expect("lock reader release gate");
                while !*released {
                    released = wake.wait(released).expect("wait for reader release gate");
                }
                Ok(())
            })
        })
    };

    let both_entered = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        entered_rx
            .recv()
            .await
            .expect("first reader should enter its closure");
        entered_rx
            .recv()
            .await
            .expect("second reader should enter its closure");
    })
    .await;

    // Always release blocking workers before inspecting the timeout result.
    // A failed pool checkout must not leave the peer parked beyond this test.
    {
        let (released, wake) = &*release;
        *released.lock().expect("release reader gate") = true;
        wake.notify_all();
    }

    first
        .await
        .expect("first reader task should join")
        .expect("first reader should succeed");
    second
        .await
        .expect("second reader task should join")
        .expect("second reader should succeed");
    both_entered.expect("two readers should enter read closures concurrently");

    assert_eq!(
        entered.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "both reader closures should have entered before either returned"
    );
}

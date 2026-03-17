use crate::common;
use agentos_audit::AuditEventType;
use serial_test::serial;

/// `Kernel::shutdown()` must write exactly one `KernelShutdown` audit entry
/// (reason: api_shutdown) before cancelling the token.
///
/// With the `AtomicBool` guard, the `cancelled()` arm in `run()` will see the
/// flag already set and skip writing a second entry — so the count is exactly 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_shutdown_api_writes_kernel_shutdown_audit() {
    let (kernel, _client, _tmp, run_handle) = common::setup_kernel().await;

    // No KernelShutdown entries before shutdown
    let before = kernel
        .audit
        .query_by_type(AuditEventType::KernelShutdown, 10)
        .unwrap();
    assert!(
        before.is_empty(),
        "Expected no KernelShutdown entries before shutdown, got {}",
        before.len()
    );

    kernel.shutdown();

    // Await the run loop so the supervisor has fully exited before we assert
    run_handle.await.unwrap();

    let entries = kernel
        .audit
        .query_by_type(AuditEventType::KernelShutdown, 10)
        .unwrap();

    assert_eq!(
        entries.len(),
        1,
        "Expected exactly 1 KernelShutdown entry after shutdown(), got {}",
        entries.len()
    );
    assert_eq!(
        entries[0].details["reason"], "api_shutdown",
        "Expected reason 'api_shutdown', got: {}",
        entries[0].details["reason"]
    );
}

/// Cancelling the token directly must cause the supervisor `run()` loop to write
/// exactly one `KernelShutdown` entry (reason: cancellation_token) before returning.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_run_loop_writes_shutdown_audit_on_cancel() {
    let (kernel, _client, _tmp, run_handle) = common::setup_kernel().await;

    // Cancel directly — bypasses shutdown() to exercise the run() cancelled arm
    kernel.cancellation_token.cancel();

    // Await the run loop to ensure the audit write has completed
    run_handle.await.unwrap();

    let entries = kernel
        .audit
        .query_by_type(AuditEventType::KernelShutdown, 10)
        .unwrap();

    assert_eq!(
        entries.len(),
        1,
        "Expected exactly 1 KernelShutdown entry after direct cancellation, got {}",
        entries.len()
    );
    assert_eq!(
        entries[0].details["reason"], "cancellation_token",
        "Expected reason 'cancellation_token', got: {}",
        entries[0].details["reason"]
    );
}

use super::*;

#[test]
fn deployment_observation_has_a_deadline() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let result = runtime.block_on(bounded_operation(
        std::future::pending::<Result<(), String>>(),
        Duration::ZERO,
    ));
    assert!(result.unwrap_err().contains("timed out"));
}

#[test]
fn deployment_observation_preserves_reported_failure() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let result = runtime.block_on(bounded_operation(
        async { Err::<(), _>("worker failed".to_owned()) },
        Duration::from_secs(1),
    ));
    assert_eq!(result.unwrap_err(), "worker failed");
}

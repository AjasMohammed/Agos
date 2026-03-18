// Integration tests for TestHarness boot and basic functionality.
// These tests boot a real kernel in a temp directory using the mock provider.
// Serial execution is required because all tests share the model cache directory.
use agentos_agent_tester::feedback::FeedbackCollector;
use agentos_agent_tester::harness::TestHarness;
use agentos_agent_tester::scenarios::{agent_lifecycle, builtin_scenarios, ScenarioOutcome};
use serial_test::serial;

#[tokio::test]
#[serial]
async fn test_boot_registers_agent() {
    let mut harness = TestHarness::boot("mock", "mock-model", None)
        .await
        .expect("Harness boot should succeed");

    {
        let registry = harness.kernel.agent_registry.read().await;
        let agent = registry.get_by_name("test-agent");
        assert!(
            agent.is_some(),
            "test-agent should be registered after boot"
        );
    } // drop registry lock before shutdown

    harness.shutdown().await;
}

#[tokio::test]
#[serial]
async fn test_boot_wires_active_llm() {
    let mut harness = TestHarness::boot("mock", "mock-model", None)
        .await
        .expect("Harness boot should succeed");

    {
        let active = harness.kernel.active_llms.read().await;
        assert!(
            active.contains_key(&harness.agent_id),
            "Active LLMs should contain test-agent after boot"
        );
    } // drop lock before shutdown

    harness.shutdown().await;
}

#[tokio::test]
#[serial]
async fn test_shutdown_completes_without_panic() {
    let mut harness = TestHarness::boot("mock", "mock-model", None)
        .await
        .expect("Harness boot should succeed");

    // Should not panic
    harness.shutdown().await;
}

#[tokio::test]
#[serial]
async fn test_mock_scenario_completes() {
    let mut harness = TestHarness::boot("mock", "mock-model", None)
        .await
        .expect("Harness boot should succeed");

    let scenarios = builtin_scenarios(5);
    let scenario = scenarios.first().expect("At least one builtin scenario");

    let mut collector = FeedbackCollector::new();
    // Use the scenario's own mock responses so the goal keyword is guaranteed to be present.
    let result = harness
        .run_scenario_with_mock(scenario, agent_lifecycle::mock_responses(), &mut collector)
        .await;

    assert_eq!(
        result.outcome,
        ScenarioOutcome::Complete,
        "Mock scenario should complete (goal keywords present in mock response)"
    );
    assert!(
        result.feedback_count >= 1,
        "At least one feedback entry expected"
    );
    assert!(result.turns_used >= 1, "At least one turn should be used");

    harness.shutdown().await;
}

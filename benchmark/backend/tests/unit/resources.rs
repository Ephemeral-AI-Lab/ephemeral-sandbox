use sandbox_benchmark::resources::{
    counter_delta, metric_definition, AggregationRule, Availability, MetricKind, MetricScope,
    MetricUnit, ResourceReading,
};

#[test]
fn unavailable_measurements_are_explicit_and_never_encoded_as_zero() {
    let unavailable = Availability::<u64>::Unavailable {
        source: "cgroup-v2".to_owned(),
        reason: "counter_missing".to_owned(),
    };

    assert_ne!(unavailable, Availability::Available { value: 0 });
    let json = serde_json::to_value(&unavailable).expect("serialize availability");
    assert_eq!(json["availability"], "unavailable");
    assert_eq!(json["source"], "cgroup-v2");
    assert_eq!(json["reason"], "counter_missing");
    assert!(json.get("value").is_none());

    let persisted = ResourceReading {
        schema_version: 1,
        metric_id: "sandbox_cpu_time_ns".to_owned(),
        metric_semantic_revision: 1,
        unit: MetricUnit::Nanoseconds,
        scope: MetricScope::Sandbox,
        kind: MetricKind::MonotonicCounter,
        aggregation: AggregationRule::Delta,
        source: "cgroup-v2".to_owned(),
        monotonic_offset_ns: 99,
        sampled: false,
        value: Availability::Unavailable {
            source: "cgroup-v2".to_owned(),
            reason: "counter_missing".to_owned(),
        },
    };
    let json = serde_json::to_value(&persisted).expect("serialize resource reading");
    assert_eq!(json["unit"], "nanoseconds");
    assert_eq!(json["scope"], "sandbox");
    assert_eq!(json["kind"], "monotonic_counter");
    assert_eq!(json["aggregation"], "delta");
    assert_eq!(json["value"]["availability"], "unavailable");
    assert!(json["value"].get("value").is_none());
}

#[test]
fn counter_deltas_report_resets_and_propagate_unavailable_boundaries() {
    assert_eq!(
        counter_delta(
            &Availability::Available { value: 10 },
            &Availability::Available { value: 17 },
            "sandbox.cpu.stat",
        ),
        Availability::Available { value: 7 }
    );
    assert_eq!(
        counter_delta(
            &Availability::Available { value: 17 },
            &Availability::Available { value: 10 },
            "sandbox.cpu.stat",
        ),
        Availability::Unavailable {
            source: "sandbox.cpu.stat".to_owned(),
            reason: "counter_reset_or_regression".to_owned(),
        }
    );
    assert_eq!(
        counter_delta(
            &Availability::Unavailable {
                source: "baseline-probe".to_owned(),
                reason: "permission_denied".to_owned(),
            },
            &Availability::Available { value: 10 },
            "sandbox.cpu.stat",
        ),
        Availability::Unavailable {
            source: "baseline-probe".to_owned(),
            reason: "baseline_unavailable:permission_denied".to_owned(),
        }
    );
    assert_eq!(
        counter_delta(
            &Availability::Available { value: 10 },
            &Availability::Unavailable {
                source: "final-probe".to_owned(),
                reason: "sandbox_gone".to_owned(),
            },
            "sandbox.cpu.stat",
        ),
        Availability::Unavailable {
            source: "final-probe".to_owned(),
            reason: "final_unavailable:sandbox_gone".to_owned(),
        }
    );
}

#[test]
fn metric_definitions_pin_units_scopes_and_aggregation_semantics() {
    let cpu = metric_definition("sandbox_cpu_time_ns").expect("CPU metric definition");
    assert_eq!(cpu.semantic_revision, 1);
    assert_eq!(cpu.unit, MetricUnit::Nanoseconds);
    assert_eq!(cpu.scope, MetricScope::Sandbox);
    assert_eq!(cpu.kind, MetricKind::MonotonicCounter);
    assert_eq!(cpu.aggregation, AggregationRule::Delta);

    let memory = metric_definition("runner_rss_bytes").expect("memory metric definition");
    assert_eq!(memory.unit, MetricUnit::Bytes);
    assert_eq!(memory.scope, MetricScope::Runner);
    assert_eq!(memory.kind, MetricKind::Gauge);
    assert_eq!(memory.aggregation, AggregationRule::Maximum);
    assert!(metric_definition("unknown.metric").is_none());
}

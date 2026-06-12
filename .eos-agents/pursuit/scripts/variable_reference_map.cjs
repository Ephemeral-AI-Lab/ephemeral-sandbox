function create_variable_reference_map(ctx) {
  const pursuit = ctx.pursuit_context.pursuit;
  const current_leg =
    pursuit.legs.find((leg) => leg.id === ctx.current.leg_id) ?? null;
  const previous_leg =
    pursuit.legs
      .filter((leg) => current_leg && leg.sequence < current_leg.sequence)
      .at(-1) ?? null;
  const all_attempts = current_leg?.attempts ?? [];
  const current_attempt =
    all_attempts.find((attempt) => attempt.id === ctx.current.attempt_id) ?? null;
  const previous_attempt =
    all_attempts
      .filter((attempt) => current_attempt && attempt.sequence < current_attempt.sequence)
      .at(-1) ?? null;
  const last_attempt = all_attempts.at(-1) ?? null;
  const same_version_work_items =
    current_leg === null || current_attempt === null
      ? []
      : current_leg.attempts
          .filter((attempt) => attempt.is_consistent_with_leg_goal)
          .flatMap((attempt) => attempt.work_items)
          .filter(
            (item) =>
              current_attempt.leg_goal_version !== null &&
              item.leg_goal_version === current_attempt.leg_goal_version,
          );
  const current_work_item =
    "work_item_id" in ctx.current
      ? current_attempt?.work_items.find(
          (item) => item.id === ctx.current.work_item_id,
        ) ?? null
      : null;
  const dependencies = current_work_item
    ? current_work_item.depends_on.map(
        (id) => same_version_work_items.find((item) => item.id === id) ?? { id },
      )
    : [];

  const attempt_outcome = (attempt) =>
    attempt === null
      ? null
      : {
          attempt_id: attempt.id,
          status: attempt.status,
          failure_reasons: attempt.failure_reasons,
          plan_summary: attempt.plan.summary,
          is_consistent_with_leg_goal: attempt.is_consistent_with_leg_goal,
          context_path: attempt.context_path,
          outcome: attempt.outcome,
          leg_goal_version: attempt.leg_goal_version,
          work_items: attempt.work_items.map((item) => ({
            id: item.id,
            agent_name: item.agent_name,
            title: item.title,
            status: item.status,
            summary: item.summary,
            outcome: item.outcome,
            context_path: item.context_path,
          })),
        };
  const leg_outcome = (leg) => (leg === null ? null : leg.outcome);

  return {
    kind: ctx.kind,

    pursuit_id: pursuit.id,
    pursuit_status: pursuit.status,
    pursuit_goal: pursuit.goal,
    pursuit_leg_goal_mode: pursuit.leg_goal_mode,
    pursuit_predefined_leg_count: pursuit.predefined_leg_count,
    pursuit_context_path: pursuit.context_path,
    pursuit_outcome: pursuit.outcome,

    current_leg_id: current_leg?.id ?? null,
    current_leg_sequence: current_leg?.sequence ?? null,
    current_leg_origin: current_leg?.origin ?? null,
    current_leg_status: current_leg?.status ?? null,
    current_leg_goal: current_leg?.leg_goal ?? null,
    current_leg_goal_version: current_leg?.leg_goal_version ?? null,
    current_leg_goal_provenance: current_leg?.leg_goal_provenance ?? null,
    current_leg_goal_mutatable: current_leg?.is_leg_goal_mutatable ?? null,
    current_leg_next_leg_goal: current_leg?.next_leg_goal ?? null,
    current_leg_max_attempts: current_leg?.max_attempts ?? null,
    current_leg_context_path: current_leg?.context_path ?? null,
    current_leg_outcome: leg_outcome(current_leg),

    previous_leg_id: previous_leg?.id ?? null,
    previous_leg_sequence: previous_leg?.sequence ?? null,
    previous_leg_status: previous_leg?.status ?? null,
    previous_leg_goal: previous_leg?.leg_goal ?? null,
    previous_leg_next_leg_goal: previous_leg?.next_leg_goal ?? null,
    previous_leg_context_path: previous_leg?.context_path ?? null,
    previous_leg_outcome: leg_outcome(previous_leg),

    current_attempt_id: current_attempt?.id ?? null,
    current_attempt_sequence: current_attempt?.sequence ?? null,
    current_attempt_status: current_attempt?.status ?? null,
    current_attempt_failure_reasons: current_attempt?.failure_reasons ?? [],
    current_attempt_is_consistent_with_leg_goal:
      current_attempt?.is_consistent_with_leg_goal ?? null,
    current_attempt_context_path: current_attempt?.context_path ?? null,
    current_attempt_outcome: attempt_outcome(current_attempt),
    current_attempt_work_items: current_attempt?.work_items ?? [],

    previous_attempt_id: previous_attempt?.id ?? null,
    previous_attempt_sequence: previous_attempt?.sequence ?? null,
    previous_attempt_status: previous_attempt?.status ?? null,
    previous_attempt_failure_reasons: previous_attempt?.failure_reasons ?? [],
    previous_attempt_is_consistent_with_leg_goal:
      previous_attempt?.is_consistent_with_leg_goal ?? null,
    previous_attempt_context_path: previous_attempt?.context_path ?? null,
    previous_attempt_outcome: attempt_outcome(previous_attempt),

    last_attempt_id: last_attempt?.id ?? null,
    last_attempt_status: last_attempt?.status ?? null,
    last_attempt_failure_reasons: last_attempt?.failure_reasons ?? [],
    last_attempt_context_path: last_attempt?.context_path ?? null,
    last_attempt_outcome: attempt_outcome(last_attempt),

    attempts_consistent_with_leg_goal: all_attempts.filter(
      (attempt) => attempt.is_consistent_with_leg_goal,
    ),
    attempts_not_consistent_with_leg_goal: all_attempts.filter(
      (attempt) => !attempt.is_consistent_with_leg_goal,
    ),
    failed_attempts: all_attempts.filter((attempt) => attempt.status === "Failed"),
    cancelled_attempts: all_attempts.filter((attempt) => attempt.status === "Cancelled"),

    current_plan_id: current_attempt?.plan.id ?? null,
    current_plan_status: current_attempt?.plan.status ?? null,
    current_plan_summary: current_attempt?.plan.summary ?? null,
    current_plan_declared_leg_goal: current_attempt?.plan.declared_leg_goal ?? null,
    current_plan_declared_next_leg_goal:
      current_attempt?.plan.declared_next_leg_goal ?? null,
    current_plan_leg_goal_version: current_attempt?.plan.leg_goal_version ?? null,

    work_item_id: current_work_item?.id ?? null,
    work_item_agent_name: current_work_item?.agent_name ?? null,
    work_item_title: current_work_item?.title ?? null,
    assigned_work_spec: current_work_item?.spec ?? null,
    work_item_status: current_work_item?.status ?? null,
    work_item_summary: current_work_item?.summary ?? null,
    work_item_outcome: current_work_item?.outcome ?? null,
    work_item_depends_on: current_work_item?.depends_on ?? [],
    work_item_context_path: current_work_item?.context_path ?? null,
    dependency_work_items: dependencies,
    dependency_outcomes: dependencies
      .filter((item) => item.status === "Success")
      .map((item) => ({
        id: item.id,
        title: item.title ?? null,
        status: item.status ?? "Unknown",
        summary: item.summary ?? null,
        outcome: item.outcome ?? null,
      })),
  };
}

module.exports = { create_variable_reference_map };

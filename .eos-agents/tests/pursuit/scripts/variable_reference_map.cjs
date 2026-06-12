function create_variable_reference_map(ctx) {
  const pursuit = ctx.pursuit_context.pursuit;
  const current_leg = pursuit.legs.find(
    (i) => i.id === ctx.current.leg_id,
  ) ?? null;
  const all_attempts = current_leg ? current_leg.attempts : [];
  const current_attempt = all_attempts.find((a) => a.id === ctx.current.attempt_id) ?? null;
  const previous_attempt =
    all_attempts
      .filter((a) => current_attempt && a.sequence < current_attempt.sequence)
      .at(-1) ?? null;
  const all_work_items = pursuit.legs.flatMap((leg) =>
    leg.attempts.flatMap((attempt) => attempt.work_items),
  );
  const current_work_item =
    "work_item_id" in ctx.current
      ? all_work_items.find((item) => item.id === ctx.current.work_item_id) ?? null
      : null;
  const dependencies = current_work_item
    ? current_work_item.depends_on.map(
        (id) => all_work_items.find((item) => item.id === id) ?? { id },
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
          work_items: attempt.work_items.map((item) => ({
            id: item.id,
            status: item.status,
            summary: item.summary,
            outcome: item.outcome,
          })),
        };
  const goal_for_leg = (leg_id) => {
    const index = pursuit.legs.findIndex((leg) => leg.id === leg_id);
    let goal = pursuit.goal;
    for (let cursor = 1; cursor <= index; cursor += 1) {
      goal = pursuit.legs[cursor - 1].next_leg_goal ?? goal;
    }
    return goal;
  };
  return {
    kind: ctx.kind,
    pursuit_goal: goal_for_leg(ctx.current.leg_id),
    current_leg_goal: current_leg ? current_leg.leg_goal : null,
    previous_attempt_outcome: attempt_outcome(previous_attempt),
    work_item_title: current_work_item ? current_work_item.title : null,
    item_spec: current_work_item ? current_work_item.spec : null,
    dependency_outcomes: dependencies.map((item) => ({
      id: item.id,
      title: item.title ?? null,
      status: item.status ?? "Unknown",
      summary: item.summary ?? null,
      outcome: item.outcome ?? null,
    })),
  };
}

module.exports = { create_variable_reference_map };

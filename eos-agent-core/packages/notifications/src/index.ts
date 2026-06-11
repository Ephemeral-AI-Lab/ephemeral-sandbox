// The notification system in one package, layered below the engine: the
// inbox the loop drains, the loop-observer port the engine announces
// through, the notification-rule contracts, and the trigger engine that
// implements the port. The spawn-backed command runner stays with the
// shared command-spawn mechanics in @eos/tool; config loading stays with
// the runtime's other operator-config loaders.
export {
  NotificationInbox,
  systemNotificationMessage,
} from "./inbox.js";
export type { LoopObserver, TurnFacts } from "./loop-observer.js";
export { NotificationTriggerEngine } from "./trigger-engine.js";
export {
  TriggerOutputSchema,
  TriggerRuleEntrySchema,
  triggerRuleAppliesTo,
  type CommandScript,
  type TriggerCommandRunner,
  type TriggerPayload,
  type TriggerRuleEntry,
} from "./triggers.js";

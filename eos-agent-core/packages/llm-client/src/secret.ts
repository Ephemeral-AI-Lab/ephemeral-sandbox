import { inspect } from "node:util";

const REDACTED = "[redacted]";

/**
 * A wrapper that keeps a credential out of logs, json, and inspect output.
 * `expose()` is called exactly once, inside a provider constructor.
 */
export class SecretString {
  readonly #value: string;

  constructor(value: string) {
    this.#value = value;
  }

  /** Read the raw secret for an sdk client constructor. */
  expose(): string {
    return this.#value;
  }

  toString(): string {
    return REDACTED;
  }

  toJSON(): string {
    return REDACTED;
  }

  [inspect.custom](): string {
    return REDACTED;
  }
}

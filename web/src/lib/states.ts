/** The API's lifecycle states, in lifecycle order, for filters and facets. */

import type { components } from "@/api/schema";

export const HYPOTHESIS_STATES = [
  "draft",
  "queued",
  "active",
  "awaiting_human_review",
  "promoted",
  "rejected",
  "inconclusive",
  "declined",
  "failed",
  "cancelled",
] as const;

export type HypothesisState = (typeof HYPOTHESIS_STATES)[number];

/** Shown as archived: hidden by the default filter, always reachable. */
export const ARCHIVED_STATES: readonly string[] = [
  "declined",
  "rejected",
  "inconclusive",
  "failed",
  "cancelled",
];

export function isHypothesisState(value: string | null): value is HypothesisState {
  return value !== null && (HYPOTHESIS_STATES as readonly string[]).includes(value);
}

export const TRACK_STATES = ["active", "paused", "archived"] as const;

/** Allowed track transitions, as the backend enforces them. */
export const TRACK_TRANSITIONS: Record<
  string,
  readonly components["schemas"]["TrackTransitionRequestToState"][]
> = {
  active: ["paused", "archived"],
  paused: ["active", "archived"],
  archived: ["active"],
};

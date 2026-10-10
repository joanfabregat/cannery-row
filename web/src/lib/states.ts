/** The API's lifecycle states, in lifecycle order, for filters and facets. */

import type { components } from "@/api/schema";

export const UNIT_STATES = [
  "queued",
  "active",
  "documenting",
  "deciding",
  "promoted",
  "rejected",
  "inconclusive",
  "failed",
  "cancelled",
] as const;

export type UnitState = (typeof UNIT_STATES)[number];

/** Shown as archived: hidden by the default filter, always reachable. */
export const ARCHIVED_STATES: readonly string[] = [
  "rejected",
  "inconclusive",
  "failed",
  "cancelled",
];

export function isUnitState(value: string | null): value is UnitState {
  return value !== null && (UNIT_STATES as readonly string[]).includes(value);
}

export const TRACK_STATES = ["planning", "active", "paused", "archived"] as const;

/** Allowed track transitions, as the backend enforces them. */
export const TRACK_TRANSITIONS: Record<
  string,
  readonly components["schemas"]["TrackTransitionRequestToState"][]
> = {
  // A planning track becomes active when its first plan is approved.
  planning: ["paused", "archived"],
  active: ["paused", "archived"],
  paused: ["active", "archived"],
  archived: ["active"],
};

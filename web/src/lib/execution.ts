import type { Artifact, AttemptFailure, Job } from "@/api/types";

/**
 * How a stage ran: a track's or an attempt's pinned workflow, the runner
 * that claimed a run, and the logs each step left. Everything comes from
 * the API's own fields; nothing is guessed.
 */

export interface WorkflowStep {
  name: string;
  revision: string;
}

/** The steps of a workflow (`{steps: [{name, revision}]}`), in order; none when absent. */
export function workflowSteps(
  workflow: Record<string, unknown> | null | undefined,
): WorkflowStep[] {
  const steps = workflow?.steps;
  if (!Array.isArray(steps)) return [];
  return steps.flatMap((step: unknown): WorkflowStep[] => {
    if (typeof step !== "object" || step === null) return [];
    const { name, revision } = step as { name?: unknown; revision?: unknown };
    if (typeof name !== "string") return [];
    return [
      {
        name,
        revision:
          typeof revision === "string" || typeof revision === "number" ? String(revision) : "",
      },
    ];
  });
}

/** "fixture-experiment (revision 1)". */
export function stepText(step: { name: string; revision: string | number }): string {
  const revision = String(step.revision);
  return revision ? `${step.name} (revision ${revision})` : step.name;
}

/**
 * The runner's name from a client label: `token:<name>` names the token a
 * runner claimed with (an MCP client adds `;` and its user agent). Any other
 * label is shown as it is.
 */
export function runnerName(viaClient: string | null | undefined): string | null {
  return runnerLabel(viaClient)?.name ?? null;
}

/**
 * The runner's name, and whether it is a token's name (`token:<name>`) or
 * another client label, which is shown plainly, never as a token.
 */
export function runnerLabel(
  viaClient: string | null | undefined,
): { name: string; token: boolean } | null {
  if (!viaClient) return null;
  if (!viaClient.startsWith("token:")) return { name: viaClient, token: false };
  const name = viaClient.slice("token:".length).split(";")[0]?.trim();
  return name ? { name, token: true } : null;
}

/** The roles a runner gives the logs it uploads. */
export const LOG_ROLES: readonly string[] = ["step_log", "setup_log", "validator_log"];

export function isLog(artifact: Artifact): boolean {
  return LOG_ROLES.includes(artifact.role);
}

export interface StepLogs {
  step: string;
  logs: Artifact[];
}

/**
 * A verify run's logs by step, in the order of the run's steps.
 * A run's outputs live at `<output_prefix><step>/<role>/<file>`.
 */
export function jobLogsByStep(job: Job): StepLogs[] {
  const groups = new Map<string, Artifact[]>(job.steps.map((s) => [s.name, []]));
  for (const output of job.outputs) {
    if (!isLog(output)) continue;
    const key = output.storage.key;
    const relative = key.startsWith(job.output_prefix) ? key.slice(job.output_prefix.length) : key;
    const step = relative.split("/")[0] ?? "";
    groups.set(step, [...(groups.get(step) ?? []), output]);
  }
  return [...groups.entries()]
    .filter(([, logs]) => logs.length > 0)
    .map(([step, logs]) => ({ step, logs }));
}

/** The artifacts a failure's log references point to, among those the page holds. */
export function failureLogs(failure: AttemptFailure, artifacts: Artifact[]): Artifact[] {
  const byKey = new Map(artifacts.map((a) => [a.storage.key, a]));
  return (failure.log_refs ?? []).flatMap((ref) => {
    const found = byKey.get(ref.key);
    return found ? [found] : [];
  });
}

/** The failing step a failure names, if any. */
export function failureStep(failure: AttemptFailure): string | null {
  const step = failure.details.step;
  return typeof step === "string" && step ? step : null;
}

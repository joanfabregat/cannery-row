import { useAttemptJobs } from "@/api/queries";
import type { Artifact, AttemptDetail, Job, Track } from "@/api/types";
import { ArtifactList } from "@/components/evidence";
import { QueryView } from "@/components/query-state";
import { Fact, Section, SubSection } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import {
  isLog,
  jobLogsByStep,
  runnerLabel,
  stepText,
  type WorkflowStep,
  workflowSteps,
} from "@/lib/execution";
import { formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { usePeople } from "@/projects/use-people";
import { usePermissions } from "@/projects/use-permissions";

/**
 * How a track runs its experiments, and how each stage of an attempt ran:
 * the experiment (by an outside agent or a runner's workflow) and the
 * verification, with who claimed each run and the logs it left.
 */

const MODE_WORDS: Record<string, string> = {
  agent:
    "An outside agent claims each hypothesis through the API or MCP, runs the experiment and submits the result.",
  workflow:
    "A Cannery Row runner claims each hypothesis with an experimenter service account and runs the workflow below, then submits the result.",
};

export function WorkflowStepList({ steps }: { steps: WorkflowStep[] }) {
  if (steps.length === 0) return <p className="text-sm text-muted-foreground">No step.</p>;
  return (
    <ol className="flex list-decimal flex-col gap-1 pl-5 text-sm">
      {steps.map((step, index) => (
        <li key={`${String(index)}-${step.name}`}>{stepText(step)}</li>
      ))}
    </ol>
  );
}

/** A track's execution mode and, in workflow mode, the experiment steps it pins. */
export function TrackExecution({ track }: { track: Track }) {
  const steps = workflowSteps(track.workflow);
  return (
    <Section title="Execution" description="How this track's hypotheses are run.">
      <div className="flex flex-col gap-4">
        <dl className="grid gap-4 md:grid-cols-2">
          <Fact term="Mode">
            <span className="font-medium">{label("trackMode", track.mode)}</span>
            {MODE_WORDS[track.mode] ? `: ${MODE_WORDS[track.mode] ?? ""}` : null}
          </Fact>
        </dl>
        {track.mode === "workflow" ? (
          <div className="flex flex-col gap-2">
            <h3 className="text-sm font-medium">Workflow</h3>
            <p className="text-sm text-muted-foreground">
              The experiment steps, in order, at the revisions this track pins.
            </p>
            <WorkflowStepList steps={steps} />
          </div>
        ) : null}
      </div>
    </Section>
  );
}

function LogList({ project, logs }: { project: string; logs: Artifact[] }) {
  const { canDownloadArtifacts } = usePermissions();
  if (logs.length === 0) return <p className="text-sm text-muted-foreground">No log.</p>;
  if (!canDownloadArtifacts) {
    return (
      <p className="text-sm text-muted-foreground">
        {logs.length === 1 ? "One log" : `${String(logs.length)} logs`}: members can download them.
      </p>
    );
  }
  return <ArtifactList project={project} artifacts={logs} />;
}

function Runner({ viaClient }: { viaClient: string | null }) {
  const runner = runnerLabel(viaClient);
  return runner ? (
    <>
      {runner.token ? "The runner holding token " : "The runner "}
      <span className="font-medium">{runner.name}</span>
    </>
  ) : (
    <>—</>
  );
}

function ExperimentStage({ project, attempt }: { project: string; attempt: AttemptDetail }) {
  const workflow = attempt.mode === "workflow";
  const logs = attempt.artifacts.filter(isLog);
  return (
    <SubSection
      title="Experiment run"
      description={
        workflow
          ? "A Cannery Row runner ran the track's workflow and submitted the result."
          : "An outside agent ran the experiment and submitted the result."
      }
    >
      <div className="flex flex-col gap-4">
        <dl className="grid gap-4 sm:grid-cols-2">
          <Fact term="Mode">{label("trackMode", attempt.mode)}</Fact>
          <Fact term="Run by">
            {workflow ? (
              <Runner viaClient={attempt.via_client} />
            ) : (
              <>
                {attempt.claimed_by.kind === "service" ? "An agent" : "A person"} through{" "}
                {label("channel", attempt.via_channel)}
              </>
            )}
          </Fact>
        </dl>
        {workflow ? (
          <>
            <div className="flex flex-col gap-2">
              <h4 className="text-sm font-medium">Workflow</h4>
              <WorkflowStepList steps={workflowSteps(attempt.workflow)} />
            </div>
            <div className="flex flex-col gap-2">
              <h4 className="text-sm font-medium">Logs</h4>
              <LogList project={project} logs={logs} />
            </div>
          </>
        ) : (
          <p className="text-sm text-muted-foreground">
            Cannery Row keeps no logs of an outside agent's run.
          </p>
        )}
      </div>
    </SubSection>
  );
}

/** Who claimed a verify run: a runner's token, an agent, or a researcher. */
function Verifier({ job }: { job: Job }) {
  const people = usePeople();
  if (job.state === "pending") return <>Not claimed yet</>;
  if (job.performer === "runner") return <Runner viaClient={job.via_client} />;
  if (job.claimed_by_user) return <>{people(job.claimed_by_user)}</>;
  return <>An agent</>;
}

function JobRun({ project, job }: { project: string; job: Job }) {
  const byStep = jobLogsByStep(job);
  return (
    <li
      aria-label={`Run ${String(job.run_number)}`}
      className="flex flex-col gap-3 rounded-md border p-3"
    >
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span className="font-medium">Run {job.run_number}</span>
        <StatusChip domain="job" value={job.state} />
        <span className="text-muted-foreground">{label("jobOrigin", job.origin)}</span>
      </div>
      <dl className="grid gap-4 sm:grid-cols-2">
        <Fact term="Performed by">{label("performer", job.performer)}</Fact>
        {job.verifier ? <Fact term="Verifier">{job.verifier}</Fact> : null}
        <Fact term="Run by">
          <Verifier job={job} />
        </Fact>
        <Fact term="Finished">{formatDateTime(job.finished_at)}</Fact>
        <Fact term="Steps">{job.steps.map(stepText).join(", ") || "—"}</Fact>
        {job.error_reason ? (
          <Fact term="What went wrong">
            {job.error_step ? `${job.error_step}: ` : ""}
            {job.error_reason}
          </Fact>
        ) : null}
      </dl>
      <div className="flex flex-col gap-2">
        <h5 className="text-sm font-medium">Logs</h5>
        {byStep.length === 0 ? (
          <p className="text-sm text-muted-foreground">No log.</p>
        ) : (
          <ul className="flex flex-col gap-2">
            {byStep.map(({ step, logs }) => (
              <li key={step} className="flex flex-col gap-1">
                <span className="text-xs font-medium text-muted-foreground">Step {step}</span>
                <LogList project={project} logs={logs} />
              </li>
            ))}
          </ul>
        )}
      </div>
    </li>
  );
}

/** Each stage of an attempt, who or what ran it, and the logs it left. */
export function AttemptExecution({
  project,
  number,
  attempt,
}: {
  project: string;
  number: number;
  attempt: AttemptDetail;
}) {
  const jobs = useAttemptJobs(project, number, attempt.sequence);
  return (
    <Section
      title="How it ran"
      description="Each stage, what ran it and the logs it left: the experiment, then the verification."
    >
      <div className="flex flex-col gap-4">
        <ExperimentStage project={project} attempt={attempt} />
        <QueryView query={jobs}>
          {(page) => {
            const runs = page.items.filter((j) => j.phase === "verify");
            return (
              <SubSection
                title="Verify runs"
                description="Each re-runs the result, measures it and applies the project's policy. A runner, an agent or a researcher who did not run the attempt performs it."
              >
                {runs.length === 0 ? (
                  <p className="text-sm text-muted-foreground">No verify run yet.</p>
                ) : (
                  <ul className="flex flex-col gap-3">
                    {runs.map((job) => (
                      <JobRun key={job.id} project={project} job={job} />
                    ))}
                  </ul>
                )}
              </SubSection>
            );
          }}
        </QueryView>
      </div>
    </Section>
  );
}

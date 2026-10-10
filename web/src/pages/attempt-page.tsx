import { Link, useParams } from "react-router";

import { useAttempt, useAttemptJobs, useUnit, useReport } from "@/api/queries";
import type { AttemptDetail, AttemptFailure, Report } from "@/api/types";
import { CommentsSection } from "@/components/comments";
import { ArtifactList, ReportSection, VerificationSection } from "@/components/evidence";
import { AttemptExecution } from "@/components/execution";
import { AttemptConversation } from "@/components/messages";
import { ImportedBadge } from "@/components/imported-badge";
import { PageHeader } from "@/components/page-header";
import { ProjectPage } from "@/components/project-page";
import { EmptyState, LoadError, Loading } from "@/components/query-state";
import { Collapsible, Fact, RawJson, Section } from "@/components/section";
import { StatusChip } from "@/components/status-chip";
import { failureLogs, failureStep, runnerLabel } from "@/lib/execution";
import { formatDateTime } from "@/lib/format";
import { label } from "@/lib/labels";
import { parseNumber } from "@/lib/navigation";
import { unitPath } from "@/lib/paths";
import type { Project } from "@/projects/project-context";
import { usePermissions } from "@/projects/use-permissions";

export function AttemptPage() {
  const params = useParams();
  const number = parseNumber(params.number);
  const sequence = parseNumber(params.sequence);
  if (number === null || sequence === null) {
    return (
      <>
        <PageHeader title="Attempt not found" />
        <EmptyState>This address does not name an attempt.</EmptyState>
      </>
    );
  }
  return (
    <ProjectPage>
      {(project) => <AttemptView project={project} number={number} sequence={sequence} />}
    </ProjectPage>
  );
}

function AttemptView({
  project,
  number,
  sequence,
}: {
  project: Project;
  number: number;
  sequence: number;
}) {
  const slug = project.slug;
  const unit = useUnit(slug, number);
  const attempt = useAttempt(slug, number, sequence);
  const report = useReport(slug, number, sequence);
  const { isResearcher } = usePermissions();
  const title = `Attempt #${number}.${sequence}`;

  if (attempt.isPending) {
    return (
      <>
        <PageHeader title={title} />
        <Loading />
      </>
    );
  }
  if (attempt.isError) {
    return (
      <>
        <PageHeader title={title} />
        <LoadError
          error={attempt.error}
          retry={attempt.refetch}
          notFound={`There is no attempt #${number}.${sequence} in ${project.title}.`}
        />
      </>
    );
  }
  const a = attempt.data;
  return (
    <>
      <PageHeader
        title={title}
        description={unit.data ? `An attempt of #${number} “${unit.data.title}”` : undefined}
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <StatusChip domain="attempt" value={a.state} className="text-sm" />
            <ImportedBadge origin={a.origin} sourceRef={a.source_ref} showSource />
          </div>
        }
      />
      <div className="flex flex-col gap-6">
        <p className="text-sm">
          <Link to={unitPath(number)} className="font-medium underline underline-offset-4">
            Back to unit #{number}
          </Link>
        </p>
        <AttemptFacts attempt={a} />
        {a.failures.length > 0 ? (
          <FailuresSection project={slug} number={number} attempt={a} />
        ) : null}
        {report.isPending ? (
          <Loading />
        ) : report.isError ? (
          <LoadError error={report.error} retry={report.refetch} />
        ) : report.data === null ? (
          <EmptyState>This attempt has no report yet.</EmptyState>
        ) : (
          <ReportBlocks project={slug} report={report.data} />
        )}
        {a.origin === "imported" ? null : (
          <AttemptExecution project={slug} number={number} attempt={a} />
        )}
        {a.origin === "imported" || a.mode === "workflow" ? null : (
          <AttemptConversation
            project={slug}
            number={number}
            sequence={sequence}
            state={a.state}
            isResearcher={isResearcher}
          />
        )}
        <FilesSection project={slug} attempt={a} />
        <CommentsSection project={slug} target={{ number, sequence }} />
        <AttemptDetails attempt={a} />
      </div>
    </>
  );
}

function ReportBlocks({ project, report }: { project: string; report: Report }) {
  return (
    <>
      <ReportSection report={report} />
      <VerificationSection project={project} report={report} />
    </>
  );
}

/** A failure's own words, its failing step and the logs the runner reported with it. */
function FailuresSection({
  project,
  number,
  attempt,
}: {
  project: string;
  number: number;
  attempt: AttemptDetail;
}) {
  const { canDownloadArtifacts } = usePermissions();
  const jobs = useAttemptJobs(project, number, attempt.sequence);
  const files = [...attempt.artifacts, ...(jobs.data?.items.flatMap((j) => j.outputs) ?? [])];
  return (
    <Section title="What went wrong" description="Failures recorded on this attempt.">
      <ul className="flex flex-col gap-3">
        {attempt.failures.map((failure, index) => (
          <FailureItem
            key={index}
            project={project}
            failure={failure}
            logs={canDownloadArtifacts ? failureLogs(failure, files) : []}
          />
        ))}
      </ul>
    </Section>
  );
}

function FailureItem({
  project,
  failure,
  logs,
}: {
  project: string;
  failure: AttemptFailure;
  logs: AttemptDetail["artifacts"];
}) {
  const step = failureStep(failure);
  return (
    <li className="flex flex-col gap-2 rounded-md border p-3 text-sm">
      <div>
        <p className="font-medium">
          {label("stage", failure.stage)}
          {step ? `, step ${step}` : ""}: {failure.reason}
        </p>
        <p className="mt-1 text-xs text-muted-foreground">
          {formatDateTime(failure.created_at)} · code {failure.code}
          {failure.requeued ? " · the unit was queued again automatically" : ""}
        </p>
      </div>
      {logs.length > 0 ? (
        <div className="flex flex-col gap-1">
          <p className="text-xs font-medium text-muted-foreground">Logs of the failing run</p>
          <ArtifactList project={project} artifacts={logs} />
        </div>
      ) : null}
    </li>
  );
}

const IMPORTED_FACTS = [
  ["Run label", "label"],
  ["Configuration", "config"],
  ["Code revision", "source_revision"],
  ["Notes", "notes"],
] as const;

/** What an imported history states about a run, as (term, text) pairs. */
function importedFacts(imported: Record<string, unknown> | null): [string, string][] {
  if (!imported) return [];
  return IMPORTED_FACTS.flatMap(([term, key]): [string, string][] => {
    const value = imported[key];
    return typeof value === "string" ? [[term, value]] : [];
  });
}

function RunnerWords({ viaClient }: { viaClient: string | null }) {
  const runner = runnerLabel(viaClient);
  if (!runner) return <>A workflow runner</>;
  return <>{`A workflow runner (${runner.token ? "token " : ""}${runner.name})`}</>;
}

function AttemptFacts({ attempt }: { attempt: AttemptDetail }) {
  return (
    <Section title="Summary">
      <dl className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <Fact term="Started">{formatDateTime(attempt.claimed_at)}</Fact>
        <Fact term="Submitted">{formatDateTime(attempt.submitted_at)}</Fact>
        <Fact term="Finished">{formatDateTime(attempt.finished_at)}</Fact>
        <Fact term="Track">{attempt.track}</Fact>
        <Fact term="Started by">
          {attempt.origin === "imported" ? (
            "Imported"
          ) : attempt.mode === "workflow" ? (
            <RunnerWords viaClient={attempt.via_client} />
          ) : (
            <>
              {attempt.claimed_by.kind === "service" ? "An agent" : "A person"} through{" "}
              {label("channel", attempt.via_channel)}
            </>
          )}
        </Fact>
        {importedFacts(attempt.imported).map(([term, value]) => (
          <Fact key={term} term={term}>
            {value}
          </Fact>
        ))}
        {attempt.predecessor_id ? (
          <Fact term="Follows">An earlier failed attempt of the same unit</Fact>
        ) : null}
      </dl>
    </Section>
  );
}

/** Viewers download a report's images only; members and above every file. */
function FilesSection({ project, attempt }: { project: string; attempt: AttemptDetail }) {
  const { canDownloadArtifacts } = usePermissions();
  const files = canDownloadArtifacts
    ? attempt.artifacts
    : attempt.artifacts.filter((a) => a.role === "report_asset");
  if (files.length === 0) return null;
  return (
    <Section
      title="Files"
      description={
        !canDownloadArtifacts
          ? "The images the report uses."
          : attempt.mode === "workflow"
            ? "Files the runner uploaded."
            : "Files the agent submitted."
      }
    >
      <ArtifactList project={project} artifacts={files} />
    </Section>
  );
}

function AttemptDetails({ attempt }: { attempt: AttemptDetail }) {
  return (
    <Collapsible summary="Details">
      <div className="flex flex-col gap-5">
        <dl className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <Fact term="Unit revision">{attempt.unit_revision}</Fact>
          <Fact term="Science revision">{attempt.science_revision}</Fact>
          <Fact term="Brief revision">{attempt.brief?.revision ?? "None"}</Fact>
          <Fact term="Lease generation">{attempt.lease_generation}</Fact>
          <Fact term="Lease expires">{formatDateTime(attempt.lease_expires_at)}</Fact>
          <Fact term="Client">{attempt.via_client ?? "—"}</Fact>
          <Fact term="Identifier">
            <code className="text-xs break-all">{attempt.id}</code>
          </Fact>
        </dl>
        {attempt.artifacts.length > 0 ? (
          <div className="flex flex-col gap-2">
            <h3 className="text-sm font-medium">File checksums</h3>
            <ul className="flex flex-col gap-1 text-xs">
              {attempt.artifacts.map((a) => (
                <li key={a.id} className="break-all">
                  {a.storage.key} · {a.media_type} · sha256 <code>{a.sha256}</code>
                </li>
              ))}
            </ul>
          </div>
        ) : null}
        {attempt.producer ? <RawJson value={attempt.producer} label="Producer" /> : null}
        {attempt.claimed_sheet ? (
          <RawJson value={attempt.claimed_sheet} label="Claimed result" />
        ) : null}
      </div>
    </Collapsible>
  );
}

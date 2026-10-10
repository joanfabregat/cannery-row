import type { Schemas } from "./client";

/** Friendly names for the generated API types the pages use. */

export type HypothesisSummary = Schemas["HypothesisSummary"];
export type Hypothesis = Schemas["HypothesisOut"];
export type HypothesisPage = Schemas["HypothesisPage"];
export type HypothesisReview = Schemas["cannery_row__hypotheses__routes__ReviewCaseOut"];
export type Decision = Schemas["DecisionOut"];
export type Link = Schemas["LinkOut"];
export type Revision = Schemas["RevisionOut"];

export type Attempt = Schemas["AttemptOut"];
export type AttemptDetail = Schemas["AttemptDetail"];
export type AttemptFailure = Schemas["cannery_row__attempts__routes__FailureOut"];
export type Artifact = Schemas["ArtifactOut"];
export type Job = Schemas["JobOut"];

export type Report = Schemas["ReportOut"];
export type Verification = Schemas["VerificationReport"];

export type ReviewCase = Schemas["cannery_row__reviews__routes__ReviewCaseOut"];
export type Writeup = Schemas["WriteupOut"];

export type Track = Schemas["TrackOut"];
export type TrackEvent = Schemas["HistoryEvent"];

export type Plan = Schemas["PlanOut"];
export type PlanUnit = Schemas["PlanUnitOut"];
export type PlanRevision = Schemas["PlanRevisionOut"];
export type PlanCheck = Schemas["PlanCheckOut"];
export type UnitIndex = Schemas["UnitIndexOut"];

export type Comment = Schemas["CommentOut"];
export type CommentRevision = Schemas["CommentRevisionOut"];

export type SearchHit = Schemas["SearchHit"];
export type SearchResults = Schemas["SearchPage"];

export type Attention = Schemas["AttentionOut"];

export type Dashboard = Schemas["DashboardOut"];
export type ViewData = Schemas["ViewOut"];
export type Series = Schemas["Series"];
export type MetricPoint = Schemas["PointOut"];

export type Token = Schemas["TokenOut"];
export type TokenCreated = Schemas["TokenCreated"];
export type Member = Schemas["MemberOut"];
export type ServiceAccount = Schemas["ServiceAccountOut"];
export type User = Schemas["UserOut"];
export type ProjectOut = Schemas["ProjectOut"];

export type Brief = Schemas["BriefOut"];
export type BriefRevision = Schemas["BriefRevisionOut"];

/** A measurement row of a run (claimed) or of a verification report (verified). */
export interface Measurement {
  metric: string;
  value?: number;
  missing_reason?: string;
  authority: string;
  unit: string;
  direction: string;
  split: string;
  dimensions?: Record<string, string>;
  sample_count?: number;
  control_value?: number;
  uncertainty?: { method: string; lower: number; upper: number };
  /** An imported value's source: an artifact URI and JSON Pointer, or a document location. */
  source?: string;
}

/** A verifier finding that the claims disagree with the verified measurements. */
export interface Discrepancy {
  metric?: string;
  split?: string;
  dimensions?: Record<string, string>;
  claimed_value?: number;
  verified_value?: number;
  description: string;
}

/** One check of a verification verdict, as the verification report states it. */
export interface GateResult {
  id: string;
  result: string;
  detail?: string;
}

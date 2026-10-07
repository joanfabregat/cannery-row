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
export type TesterReport = Schemas["TesterReport"];
export type EvaluatorReport = Schemas["EvaluatorReport"];

export type ReviewCase = Schemas["cannery_row__reviews__routes__ReviewCaseOut"];

export type Track = Schemas["TrackOut"];
export type TrackEvent = Schemas["HistoryEvent"];

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

/** A measurement row of an evidence envelope (claimed or verified). */
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

/** A tester finding that the claimed sheet disagrees with the verified evidence. */
export interface Discrepancy {
  metric?: string;
  split?: string;
  dimensions?: Record<string, string>;
  claimed_value?: number;
  verified_value?: number;
  description: string;
}

/** One check of an evaluator verdict, as the evaluator reported it. */
export interface GateResult {
  id: string;
  result: string;
  detail?: string;
}

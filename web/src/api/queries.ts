import { keepPreviousData, useInfiniteQuery, useQuery } from "@tanstack/react-query";

import { isNotFound } from "@/lib/errors";
import type { HypothesisState } from "@/lib/states";

import { api, type Schemas, unwrap } from "./client";

/**
 * Read queries, keyed under the project so one invalidation after a write
 * refreshes everything the project's pages show.
 */

export const projectKey = (slug: string) => ["project", slug] as const;

export interface HypothesisFilters {
  track?: string | undefined;
  state?: HypothesisState | undefined;
  /** `false` hides archived states, `null` shows everything. */
  archived?: boolean | null;
  before?: number | undefined;
  limit?: number;
}

export function useHypotheses(slug: string, filters: HypothesisFilters) {
  return useQuery({
    queryKey: [...projectKey(slug), "hypotheses", filters],
    // The previous page stays up while the next loads, never another project's.
    placeholderData: (previous, previousQuery) =>
      previousQuery?.queryKey[1] === slug ? previous : undefined,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/hypotheses", {
          params: {
            path: { slug },
            query: {
              track: filters.track,
              state: filters.state ? [filters.state] : undefined,
              archived: filters.state ? undefined : (filters.archived ?? undefined),
              before: filters.before,
              limit: filters.limit ?? 25,
            },
          },
        }),
      ),
  });
}

export function useHypothesis(slug: string, number: number) {
  return useQuery({
    queryKey: [...projectKey(slug), "hypothesis", number],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/hypotheses/{number}", {
          params: { path: { slug, number } },
        }),
      ),
  });
}

export function useRevisions(slug: string, number: number, enabled = true) {
  return useQuery({
    queryKey: [...projectKey(slug), "hypothesis", number, "revisions"],
    enabled,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/hypotheses/{number}/revisions", {
          params: { path: { slug, number }, query: { limit: 200 } },
        }),
      ),
  });
}

export function useAttempts(slug: string, number: number) {
  return useQuery({
    queryKey: [...projectKey(slug), "hypothesis", number, "attempts"],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/hypotheses/{number}/attempts", {
          params: { path: { slug, number }, query: { limit: 200 } },
        }),
      ),
  });
}

export function useAttempt(slug: string, number: number, sequence: number | null) {
  return useQuery({
    queryKey: [...projectKey(slug), "hypothesis", number, "attempt", sequence],
    enabled: sequence !== null,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}", {
          params: { path: { slug, number, sequence: sequence ?? 0 } },
        }),
      ),
  });
}

export function useAttemptJobs(slug: string, number: number, sequence: number, enabled = true) {
  return useQuery({
    queryKey: [...projectKey(slug), "hypothesis", number, "attempt", sequence, "jobs"],
    enabled,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/jobs", {
          params: { path: { slug, number, sequence }, query: { limit: 200 } },
        }),
      ),
  });
}

/** The attempt's report, or `null` when it has none yet (a 404). */
export function useReport(slug: string, number: number, sequence: number | null) {
  return useQuery({
    queryKey: [...projectKey(slug), "hypothesis", number, "attempt", sequence, "report"],
    enabled: sequence !== null,
    queryFn: async () => {
      const result = await api
        .GET("/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/report", {
          params: { path: { slug, number, sequence: sequence ?? 0 } },
        })
        .catch((error: unknown) => {
          if (isNotFound(error)) return null;
          throw error;
        });
      return result === null ? null : unwrap(result);
    },
  });
}

/** The hypothesis's write-up, or `null` when it has none (a 404): it was never written up. */
export function useWriteup(slug: string, number: number, enabled = true) {
  return useQuery({
    queryKey: [...projectKey(slug), "hypothesis", number, "writeup"],
    enabled,
    queryFn: async () => {
      const result = await api
        .GET("/api/projects/{slug}/hypotheses/{number}/writeup", {
          params: { path: { slug, number } },
        })
        .catch((error: unknown) => {
          if (isNotFound(error)) return null;
          throw error;
        });
      return result === null ? null : unwrap(result);
    },
  });
}

export function useReviewCase(slug: string, caseId: string | null) {
  return useQuery({
    queryKey: [...projectKey(slug), "review-case", caseId],
    enabled: caseId !== null,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/review-cases/{case_id}", {
          params: { path: { slug, case_id: caseId ?? "" } },
        }),
      ),
  });
}

export function useAttention(slug: string) {
  return useQuery({
    queryKey: [...projectKey(slug), "attention"],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/attention", {
          params: { path: { slug }, query: { limit: 8 } },
        }),
      ),
  });
}

/** The project's current brief, or `null` when it has none yet (a 404). */
export function useBrief(slug: string) {
  return useQuery({
    queryKey: [...projectKey(slug), "brief"],
    queryFn: async () => {
      const result = await api
        .GET("/api/projects/{slug}/brief", { params: { path: { slug } } })
        .catch((error: unknown) => {
          if (isNotFound(error)) return null;
          throw error;
        });
      return result === null ? null : unwrap(result);
    },
  });
}

export function useBriefRevisions(slug: string) {
  return useQuery({
    queryKey: [...projectKey(slug), "brief", "revisions"],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/brief/revisions", {
          params: { path: { slug }, query: { limit: 200 } },
        }),
      ),
  });
}

export function useTracks(slug: string) {
  return useQuery({
    queryKey: [...projectKey(slug), "tracks"],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/tracks", {
          params: { path: { slug }, query: { limit: 200 } },
        }),
      ),
  });
}

export function useTrack(slug: string, track: string) {
  return useQuery({
    queryKey: [...projectKey(slug), "track", track],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/tracks/{track_slug}", {
          params: { path: { slug, track_slug: track } },
        }),
      ),
  });
}

/** Everything a track's page shows: the track, its plan and its history. */
export const planKey = (slug: string, track: string) =>
  [...projectKey(slug), "track", track] as const;

/**
 * One revision of a track's plan: `current` (the approved one), `draft` (the
 * open one, draft or submitted) or a number; `null` when there is none (a 404).
 */
export function usePlan(slug: string, track: string, revision: string) {
  return useQuery({
    queryKey: [...projectKey(slug), "track", track, "plan", revision],
    queryFn: async () => {
      const result = await api
        .GET("/api/projects/{slug}/tracks/{track_slug}/plans/{revision}", {
          params: { path: { slug, track_slug: track, revision } },
        })
        .catch((error: unknown) => {
          if (isNotFound(error)) return null;
          throw error;
        });
      return result === null ? null : unwrap(result);
    },
  });
}

export function usePlanRevisions(slug: string, track: string) {
  return useQuery({
    queryKey: [...projectKey(slug), "track", track, "plan", "revisions"],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/tracks/{track_slug}/plans", {
          params: { path: { slug, track_slug: track }, query: { limit: 200 } },
        }),
      ),
  });
}

export type ConcernState = "open" | "answered" | "dismissed";

/** The concerns raised about a track's plan, newest first. */
export function useTrackConcerns(slug: string, track: string) {
  return useQuery({
    queryKey: [...projectKey(slug), "track", track, "concerns"],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/tracks/{track_slug}/concerns", {
          params: { path: { slug, track_slug: track }, query: { limit: 200 } },
        }),
      ),
  });
}

/** The project's concerns in `state`, newest first. */
export function useConcerns(slug: string, state: ConcernState, enabled = true) {
  return useQuery({
    queryKey: [...projectKey(slug), "concerns", state],
    enabled,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/concerns", {
          params: { path: { slug }, query: { state, limit: 50 } },
        }),
      ),
  });
}

export function useTrackHistory(slug: string, track: string, enabled = true) {
  return useQuery({
    queryKey: [...projectKey(slug), "track", track, "history"],
    enabled,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/tracks/{track_slug}/history", {
          params: { path: { slug, track_slug: track }, query: { limit: 200 } },
        }),
      ),
  });
}

export type CommentTarget = { number: number; sequence?: number | undefined };

export function commentsKey(slug: string, target: CommentTarget) {
  return [...projectKey(slug), "comments", target.number, target.sequence ?? null] as const;
}

export function useComments(slug: string, target: CommentTarget) {
  return useQuery({
    queryKey: commentsKey(slug, target),
    queryFn: async () => {
      if (target.sequence === undefined) {
        return unwrap(
          await api.GET("/api/projects/{slug}/hypotheses/{number}/comments", {
            params: { path: { slug, number: target.number }, query: { limit: 200 } },
          }),
        );
      }
      return unwrap(
        await api.GET("/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/comments", {
          params: {
            path: { slug, number: target.number, sequence: target.sequence },
            query: { limit: 200 },
          },
        }),
      );
    },
  });
}

export function useCommentRevisions(slug: string, commentId: string, enabled: boolean) {
  return useQuery({
    queryKey: [...projectKey(slug), "comment", commentId, "revisions"],
    enabled,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/comments/{comment_id}/revisions", {
          params: { path: { slug, comment_id: commentId }, query: { limit: 200 } },
        }),
      ),
  });
}

/** The metric registry of a science revision: units and directions. */
export function useMetricCatalog(slug: string, scienceRevision: number | null) {
  return useQuery({
    queryKey: [...projectKey(slug), "metrics", "catalog", scienceRevision],
    enabled: scienceRevision !== null,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/metrics", {
          params: { path: { slug }, query: { science_revision: scienceRevision ?? undefined } },
        }),
      ),
  });
}

export function useDashboard(slug: string) {
  return useQuery({
    queryKey: [...projectKey(slug), "dashboard"],
    queryFn: async () =>
      unwrap(await api.GET("/api/projects/{slug}/dashboard", { params: { path: { slug } } })),
  });
}

export function useDashboardView(slug: string, viewId: string) {
  return useQuery({
    queryKey: [...projectKey(slug), "dashboard", "view", viewId],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/dashboard/views/{view_id}", {
          params: { path: { slug, view_id: viewId } },
        }),
      ),
  });
}

/** The agent's own claims for one metric and split, labelled "Reported by agent". */
export function useClaimedMetrics(
  slug: string,
  metric: string,
  split: string | undefined,
  enabled: boolean,
) {
  return useQuery({
    queryKey: [...projectKey(slug), "metrics", "agent_claim", metric, split ?? null],
    enabled,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/metrics/query", {
          params: {
            path: { slug },
            query: { metric, split, authority: "agent_claim", all_slices: true, limit: 200 },
          },
        }),
      ),
  });
}

/**
 * An imported history's values for one metric and split (`cannery import`,
 * docs/import.md), each with its own authority and source, a page of 200 at
 * a time: `fetchNextPage` follows `next_before`.
 */
export function useImportedMetrics(
  slug: string,
  metric: string,
  split: string | undefined,
  enabled: boolean,
) {
  return useInfiniteQuery({
    queryKey: [...projectKey(slug), "metrics", "imported", metric, split ?? null],
    enabled,
    initialPageParam: undefined as number | undefined,
    queryFn: async ({ pageParam }) =>
      unwrap(
        await api.GET("/api/projects/{slug}/metrics/query", {
          params: {
            path: { slug },
            query: {
              metric,
              split,
              authority: "imported",
              all_slices: true,
              limit: 200,
              before: pageParam,
            },
          },
        }),
      ),
    getNextPageParam: (last) => last.next_before ?? undefined,
  });
}

export interface SearchFilters {
  q: string;
  kind?: string[];
  track?: string[];
  hypothesis_state?: string[];
  verdict?: string[];
  decision?: string[];
  project?: string[];
  before?: string | undefined;
}

export function useSearch(filters: SearchFilters, enabled: boolean) {
  return useQuery({
    queryKey: ["search", filters],
    enabled,
    placeholderData: keepPreviousData,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/search", {
          params: {
            query: {
              q: filters.q,
              // The API validates each value; the page only offers known ones.
              kind: filters.kind as never,
              track: filters.track,
              hypothesis_state: filters.hypothesis_state as never,
              verdict: filters.verdict as never,
              decision: filters.decision as never,
              project: filters.project,
              before: filters.before,
              limit: 25,
            },
          },
        }),
      ),
  });
}

export function useTokens() {
  return useQuery({
    queryKey: ["tokens"],
    queryFn: async () =>
      unwrap(await api.GET("/api/tokens", { params: { query: { limit: 200 } } })),
  });
}

/** A bound on the member pages read, so a misbehaving cursor cannot loop forever. */
export const MAX_MEMBER_PAGES = 10;

/**
 * Every member of the project, across pages (at most `MAX_MEMBER_PAGES` of
 * 200): names for authors and decision makers need all of them, not the first page.
 */
export function useMembers(slug: string, enabled = true) {
  return useQuery({
    queryKey: [...projectKey(slug), "members"],
    enabled,
    queryFn: async () => {
      const items: Schemas["MemberOut"][] = [];
      let before: string | undefined;
      for (let page = 0; page < MAX_MEMBER_PAGES; page++) {
        const result = unwrap(
          await api.GET("/api/projects/{slug}/members", {
            params: { path: { slug }, query: { limit: 200, before } },
          }),
        );
        items.push(...result.items);
        if (!result.next_before) return { items, next_before: null };
        before = result.next_before;
      }
      return { items, next_before: before ?? null };
    },
  });
}

export function useServiceAccounts(slug: string, enabled = true) {
  return useQuery({
    queryKey: [...projectKey(slug), "service-accounts"],
    enabled,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/service-accounts", {
          params: { path: { slug }, query: { limit: 200 } },
        }),
      ),
  });
}

export function useServiceTokens(slug: string, name: string, enabled: boolean) {
  return useQuery({
    queryKey: [...projectKey(slug), "service-accounts", name, "tokens"],
    enabled,
    queryFn: async () =>
      unwrap(
        await api.GET("/api/projects/{slug}/service-accounts/{name}/tokens", {
          params: { path: { slug, name }, query: { limit: 200 } },
        }),
      ),
  });
}

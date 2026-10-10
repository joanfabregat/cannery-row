import type { RouteObject } from "react-router";

import { RequireAuth } from "@/auth/require-auth";
import { Loading } from "@/components/query-state";
import { AttemptPage } from "@/pages/attempt-page";
import { BriefPage } from "@/pages/brief-page";
import { HomePage } from "@/pages/home-page";
import { HypothesesPage } from "@/pages/hypotheses-page";
import { HypothesisPage } from "@/pages/hypothesis-page";
import { NotFoundPage } from "@/pages/not-found-page";
import { PlanEditPage } from "@/pages/plan-edit-page";
import { ReviewPage } from "@/pages/review-page";
import { SearchPage } from "@/pages/search-page";
import { SettingsPage } from "@/pages/settings-page";
import { SignInPage } from "@/pages/sign-in-page";
import { TrackPage } from "@/pages/track-page";
import { TracksPage } from "@/pages/tracks-page";

export const routes: RouteObject[] = [
  { path: "/sign-in", element: <SignInPage /> },
  {
    element: <RequireAuth />,
    children: [
      { index: true, element: <HomePage /> },
      { path: "brief", element: <BriefPage /> },
      { path: "tracks", element: <TracksPage /> },
      { path: "tracks/:track", element: <TrackPage /> },
      { path: "tracks/:track/plan", element: <PlanEditPage /> },
      { path: "hypotheses", element: <HypothesesPage /> },
      { path: "hypotheses/:number", element: <HypothesisPage /> },
      { path: "hypotheses/:number/review", element: <ReviewPage /> },
      { path: "hypotheses/:number/attempts/:sequence", element: <AttemptPage /> },
      {
        path: "results",
        // The chart library is loaded with the only page that draws charts.
        lazy: async () => ({ Component: (await import("@/pages/results-page")).ResultsPage }),
        HydrateFallback: Loading,
      },
      { path: "search", element: <SearchPage /> },
      { path: "settings", element: <SettingsPage /> },
      { path: "*", element: <NotFoundPage /> },
    ],
  },
];

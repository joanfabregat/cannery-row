import { MutationCache, QueryCache, QueryClient } from "@tanstack/react-query";

import { ApiError, isUnauthenticated, setCsrfRefresher, setCsrfToken } from "@/api/client";
import { fetchMe, meQueryKey } from "@/auth/session";

function isClientError(error: unknown): boolean {
  return error instanceof ApiError && error.status >= 400 && error.status < 500;
}

/**
 * One client for the app. A 401 from any query or mutation means the session
 * ended (expired or signed out elsewhere): the current identity becomes
 * "nobody", its CSRF token is dropped, and the auth guard sends the user to the
 * sign-in page. A request refused for a stale CSRF token refetches the
 * identity (and so a fresh token) and is retried once by the API client.
 */
export function createQueryClient(): QueryClient {
  const signedOut = () => {
    setCsrfToken(null);
    client.setQueryData(meQueryKey, null);
  };
  const client: QueryClient = new QueryClient({
    queryCache: new QueryCache({
      onError: (error, query) => {
        if (isUnauthenticated(error) && query.queryKey[0] !== meQueryKey[0]) signedOut();
      },
    }),
    mutationCache: new MutationCache({
      onError: (error) => {
        if (isUnauthenticated(error)) signedOut();
      },
    }),
    defaultOptions: {
      queries: {
        staleTime: 30_000,
        // A refusal (not found, forbidden, invalid) will not change on a retry.
        retry: (count, error) => !isClientError(error) && count < 2,
        refetchOnWindowFocus: false,
      },
    },
  });
  setCsrfRefresher(async () => {
    // fetchMe stores the new token; the cache follows so the page sees the same identity.
    client.setQueryData(meQueryKey, await fetchMe());
  });
  return client;
}

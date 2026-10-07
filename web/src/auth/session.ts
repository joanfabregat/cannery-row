import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";

import { api, isUnauthenticated, type Schemas, setCsrfToken, unwrap } from "@/api/client";
import { leaveApp } from "@/lib/leave-app";

export type Me = Schemas["MeOut"];

export const meQueryKey = ["me"] as const;

/** The signed-in identity, or `null` when there is no session. */
export async function fetchMe(): Promise<Me | null> {
  try {
    const me = unwrap(await api.GET("/api/me"));
    setCsrfToken(me.csrf_token ?? null);
    return me;
  } catch (error) {
    if (isUnauthenticated(error)) {
      setCsrfToken(null);
      return null;
    }
    throw error;
  }
}

export function useMe() {
  return useQuery({ queryKey: meQueryKey, queryFn: fetchMe, staleTime: 5 * 60_000 });
}

/**
 * A same-site path to come back to after signing in; anything else means home.
 * Backslashes and ASCII control characters are refused: browsers read `\` as
 * `/` and drop tabs and newlines, so `/\evil.com` or `/<TAB>/evil.com` would
 * leave the site.
 */
export function safeReturnTo(value: string | null | undefined): string {
  if (!value?.startsWith("/") || value.startsWith("//") || value.includes("\\")) return "/";
  // eslint-disable-next-line no-control-regex -- matching control characters is the point
  if (/[\u0000-\u001f\u007f]/.test(value)) return "/";
  return value;
}

/**
 * The identity provider's logout address, if it is safe to send the browser
 * there: https, or http only while the app itself is served over http (local
 * development). Anything else (a `javascript:` URL, a typo) yields `null`.
 */
export function safeLogoutUrl(
  value: string | null | undefined,
  appProtocol: string = globalThis.location.protocol,
): string | null {
  if (!value) return null;
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  if (url.protocol === "https:") return url.href;
  if (url.protocol === "http:" && appProtocol === "http:") return url.href;
  return null;
}

/** Starts the backend's OIDC login (a full-page navigation, not a fetch). */
export function signInHref(returnTo: string): string {
  return `/auth/login?return_to=${encodeURIComponent(safeReturnTo(returnTo))}`;
}

/**
 * Signs out. The local state is cleared and the user lands on the sign-in page
 * even when the request fails (an expired session, a refused CSRF token, the
 * API down): staying "signed in" in this tab would be worse.
 */
export function useSignOut() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const forget = () => {
    setCsrfToken(null);
    queryClient.setQueryData(meQueryKey, null);
    queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== meQueryKey[0] });
  };
  return useMutation({
    mutationFn: async () => unwrap(await api.POST("/auth/logout")),
    onSuccess: async (result) => {
      forget();
      const logoutUrl = safeLogoutUrl(result.logout_url);
      if (logoutUrl !== null) {
        // End the identity provider's session too; it sends the browser back to "/".
        leaveApp(logoutUrl);
        return;
      }
      await navigate("/sign-in", { replace: true });
    },
    onError: async () => {
      forget();
      await navigate("/sign-in", { replace: true });
    },
  });
}

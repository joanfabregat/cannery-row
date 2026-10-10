import { Navigate, useSearchParams } from "react-router";

import { safeReturnTo, signInHref, useMe } from "@/auth/session";
import { Logo } from "@/components/shell/logo";
import { Button } from "@/components/ui/button";
import { useDocumentTitle } from "@/lib/use-document-title";

/** A calm page with one action; the button starts the backend's OIDC login. */
export function SignInPage() {
  useDocumentTitle("Sign in");
  const [params] = useSearchParams();
  const returnTo = safeReturnTo(params.get("return_to"));
  const { data: me } = useMe();
  if (me) return <Navigate to={returnTo} replace />;
  return (
    <main
      id="main"
      className="flex min-h-dvh flex-col items-center justify-center bg-background px-4 py-12"
    >
      <div className="flex w-full max-w-sm flex-col items-center rounded-lg border bg-card p-8 text-center shadow-sm">
        <Logo className="size-40" />
        <h1 className="mt-6 text-2xl font-semibold tracking-tight">Sign in to Cannery Row</h1>
        <p className="mt-2 mb-8 text-muted-foreground">
          Research tracks, units and results, in one place for your team.
        </p>
        <Button asChild size="lg" className="w-full">
          <a href={signInHref(returnTo)}>Sign in</a>
        </Button>
        <p className="mt-4 text-xs text-muted-foreground">
          You will sign in with your organization's account.
        </p>
      </div>
    </main>
  );
}

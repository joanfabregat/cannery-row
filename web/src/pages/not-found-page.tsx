import { Link } from "react-router";

import { PageHeader } from "@/components/page-header";

export function NotFoundPage() {
  return (
    <>
      <PageHeader title="Page not found" description="This address does not match any page." />
      <Link to="/" className="font-medium underline underline-offset-4 hover:no-underline">
        Go to Home
      </Link>
    </>
  );
}

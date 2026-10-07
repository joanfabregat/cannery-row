import { cn } from "@/lib/utils";

/** The Cannery Row logo (#2). Decorative next to the product name. */
export function Logo({
  className,
  decorative = false,
}: {
  className?: string;
  decorative?: boolean;
}) {
  return (
    <img
      src="/logo.png"
      alt={decorative ? "" : "Cannery Row"}
      width={512}
      height={512}
      className={cn("size-10 shrink-0 object-contain", className)}
    />
  );
}

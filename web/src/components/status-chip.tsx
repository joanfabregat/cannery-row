import {
  ArchiveIcon,
  BadgeCheckIcon,
  CircleCheckIcon,
  CircleDashedIcon,
  CircleQuestionMarkIcon,
  CircleXIcon,
  ClockIcon,
  EyeIcon,
  HistoryIcon,
  type LucideIcon,
  MessageSquareQuoteIcon,
  OctagonMinusIcon,
  PauseIcon,
  PencilLineIcon,
} from "lucide-react";

import { type StatusDomain, type StatusIcon, statusMeta, type Tone } from "@/lib/labels";
import { cn } from "@/lib/utils";

const icons: Record<StatusIcon, LucideIcon> = {
  draft: PencilLineIcon,
  waiting: ClockIcon,
  progress: CircleDashedIcon,
  review: EyeIcon,
  success: CircleCheckIcon,
  failure: CircleXIcon,
  inconclusive: CircleQuestionMarkIcon,
  stopped: OctagonMinusIcon,
  archived: ArchiveIcon,
  verified: BadgeCheckIcon,
  claimed: MessageSquareQuoteIcon,
  paused: PauseIcon,
  imported: HistoryIcon,
};

const tones: Record<Tone, string> = {
  neutral: "bg-status-neutral-bg text-status-neutral",
  info: "bg-status-info-bg text-status-info",
  attention: "bg-status-attention-bg text-status-attention",
  success: "bg-status-success-bg text-status-success",
  danger: "bg-status-danger-bg text-status-danger",
};

/**
 * A status as a word and an icon, never color alone. `value` is the API's
 * internal name; it is shown in plain words.
 */
export function StatusChip({
  domain,
  value,
  className,
}: {
  domain: StatusDomain;
  value: string;
  className?: string;
}) {
  const meta = statusMeta(domain, value);
  const Icon = icons[meta.icon];
  return (
    <span
      data-slot="status-chip"
      data-tone={meta.tone}
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium whitespace-nowrap",
        tones[meta.tone],
        className,
      )}
    >
      <Icon className="size-3.5 shrink-0" aria-hidden="true" data-icon={meta.icon} />
      {meta.label}
    </span>
  );
}

import {
  ChartLineIcon,
  HouseIcon,
  LayersIcon,
  LightbulbIcon,
  type LucideIcon,
  SearchIcon,
  SettingsIcon,
} from "lucide-react";

export interface NavItem {
  to: string;
  label: string;
  icon: LucideIcon;
  /** One line, for the page's header. */
  description: string;
}

/** The sidebar: at most six entries, in plain words. */
export const navItems: NavItem[] = [
  {
    to: "/",
    label: "Home",
    icon: HouseIcon,
    description: "What needs your attention: reviews waiting for you, failures and running work.",
  },
  {
    to: "/tracks",
    label: "Tracks",
    icon: LayersIcon,
    description: "Research directions and the hypotheses grouped under each one.",
  },
  {
    to: "/hypotheses",
    label: "Hypotheses",
    icon: LightbulbIcon,
    description: "Every idea tried: what was tested, what happened and what was decided.",
  },
  {
    to: "/results",
    label: "Results",
    icon: ChartLineIcon,
    description: "Measured results over time, compared with the baseline.",
  },
  {
    to: "/search",
    label: "Search",
    icon: SearchIcon,
    description: "Find hypotheses, reports, decisions and comments.",
  },
  {
    to: "/settings",
    label: "Settings",
    icon: SettingsIcon,
    description: "Your access tokens and, for administrators, projects and members.",
  },
];

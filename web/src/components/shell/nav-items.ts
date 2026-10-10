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
    description:
      "What needs your attention: concerns about plans, reviews waiting for you, failures and running work.",
  },
  {
    to: "/tracks",
    label: "Tracks",
    icon: LayersIcon,
    description: "Research directions and the units grouped under each one.",
  },
  {
    to: "/units",
    label: "Units",
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
    description: "Find units, reports, decisions and comments.",
  },
  {
    to: "/settings",
    label: "Settings",
    icon: SettingsIcon,
    description: "Your access tokens and, for administrators, projects and members.",
  },
];

import { Link, NavLink } from "react-router";

import { cn } from "@/lib/utils";

import { Logo } from "./logo";
import { navItems } from "./nav-items";
import { ProjectSwitcher } from "./project-switcher";

/** Brand, project switcher and the six navigation entries; shared by the desktop and mobile layouts. */
export function SidebarContent({ onNavigate }: { onNavigate?: () => void }) {
  return (
    <div className="flex h-full flex-col gap-4 px-3 py-4">
      <Link
        to="/"
        onClick={onNavigate}
        className="flex items-center gap-2 rounded-md px-2 py-1 text-base font-semibold tracking-tight"
      >
        <Logo decorative />
        <span>Cannery Row</span>
      </Link>
      <ProjectSwitcher />
      <nav aria-label="Main">
        <ul className="flex flex-col gap-1">
          {navItems.map(({ to, label, icon: Icon }) => (
            <li key={to}>
              <NavLink
                to={to}
                end={to === "/"}
                onClick={onNavigate}
                className={({ isActive }) =>
                  cn(
                    "relative flex items-center gap-3 rounded-md px-3 py-2 text-sm transition-colors hover:bg-sidebar-accent",
                    isActive &&
                      "bg-sidebar-accent font-semibold shadow-xs before:absolute before:inset-y-1.5 before:left-0 before:w-1 before:rounded-full before:bg-sidebar-highlight",
                  )
                }
              >
                <Icon className="size-4 shrink-0" aria-hidden="true" />
                {label}
              </NavLink>
            </li>
          ))}
        </ul>
      </nav>
    </div>
  );
}

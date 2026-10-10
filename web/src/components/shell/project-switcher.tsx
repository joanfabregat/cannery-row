import { ChevronsUpDownIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useCurrentProject } from "@/projects/project-context";

/** Shown only when the user can open more than one project. */
export function ProjectSwitcher() {
  const { projects, current, select } = useCurrentProject();
  if (current === null) return null;
  if (projects.length < 2) {
    return (
      <p className="truncate px-2 text-sm text-muted-foreground" data-testid="project-name">
        {current.title}
      </p>
    );
  }
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="outline"
          className="h-auto w-full justify-between px-3 py-2"
          aria-label="Switch project"
        >
          <span className="flex min-w-0 flex-col items-start gap-0.5 leading-tight">
            <span className="text-xs font-normal text-muted-foreground">Project</span>
            <span className="max-w-full truncate">{current.title}</span>
          </span>
          <ChevronsUpDownIcon aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-60">
        <DropdownMenuLabel>Projects</DropdownMenuLabel>
        <DropdownMenuRadioGroup value={current.slug} onValueChange={select}>
          {projects.map((project) => (
            <DropdownMenuRadioItem key={project.slug} value={project.slug}>
              <span className="truncate">{project.title}</span>
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

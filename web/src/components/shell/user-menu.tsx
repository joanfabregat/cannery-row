import { LogOutIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { type Me, useSignOut } from "@/auth/session";
import { type ThemeChoice, useTheme } from "@/theme/theme-context";

function displayName(me: Me): string {
  return me.user?.display_name ?? me.user?.email ?? "Signed in";
}

function initials(name: string): string {
  const parts = name
    .replace(/@.*/, "")
    .split(/[\s._-]+/)
    .filter(Boolean);
  const letters = parts.length > 1 ? [parts[0], parts[parts.length - 1]] : parts.slice(0, 1);
  return letters.map((part) => part?.charAt(0).toUpperCase() ?? "").join("") || "?";
}

function isThemeChoice(value: string): value is ThemeChoice {
  return value === "light" || value === "dark" || value === "system";
}

/** Name, theme and sign out. */
export function UserMenu({ me }: { me: Me }) {
  const name = displayName(me);
  const { choice, setChoice } = useTheme();
  const signOut = useSignOut();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" className="gap-2 px-2" aria-label={`Account: ${name}`}>
          <span
            className="flex size-7 items-center justify-center rounded-full bg-brand-navy text-xs font-semibold text-brand-cream"
            aria-hidden="true"
          >
            {initials(name)}
          </span>
          <span className="hidden max-w-40 truncate lg:inline">{name}</span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-60">
        <DropdownMenuLabel className="flex flex-col">
          <span className="truncate">{name}</span>
          {me.user?.email && me.user.email !== name ? (
            <span className="truncate text-xs font-normal text-muted-foreground">
              {me.user.email}
            </span>
          ) : null}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
          Theme
        </DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={choice}
          onValueChange={(value) => {
            if (isThemeChoice(value)) setChoice(value);
          }}
        >
          <DropdownMenuRadioItem value="light">Light</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="dark">Dark</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="system">Match my system</DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
        <DropdownMenuSeparator />
        <DropdownMenuItem
          disabled={signOut.isPending}
          onSelect={() => {
            signOut.mutate();
          }}
        >
          <LogOutIcon aria-hidden="true" />
          Sign out
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

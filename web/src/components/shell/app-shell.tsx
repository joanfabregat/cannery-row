import { MenuIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Outlet, useLocation } from "react-router";

import type { Me } from "@/auth/session";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";

import { SearchBar } from "./search-bar";
import { SidebarContent } from "./sidebar";
import { UserMenu } from "./user-menu";

/**
 * The signed-in layout: a skip link, the left sidebar (a slide-in panel on
 * narrow screens), a header with the global search bar and the user menu, and
 * the page.
 */
export function AppShell({ me }: { me: Me }) {
  const [menuOpen, setMenuOpen] = useState(false);
  const main = useRef<HTMLElement>(null);
  const { pathname } = useLocation();
  const firstRender = useRef(true);
  // The mobile menu closed because a link was followed: focus goes to the page,
  // not back to the menu button (Radix's default when a dialog closes).
  const closedByNavigation = useRef(false);

  // After navigating, move focus to the new page so keyboard and screen reader
  // users start there, not in the sidebar.
  useEffect(() => {
    if (firstRender.current) {
      firstRender.current = false;
      return;
    }
    main.current?.focus();
  }, [pathname]);

  return (
    <div className="min-h-dvh">
      <a
        href="#main"
        onClick={(event) => {
          // Focus the page itself; not every browser moves focus on a fragment link.
          event.preventDefault();
          main.current?.focus();
        }}
        className="sr-only z-[60] rounded-md bg-primary px-4 py-2 text-primary-foreground focus:not-sr-only focus:fixed focus:top-3 focus:left-3"
      >
        Skip to content
      </a>
      <aside className="fixed inset-y-0 left-0 hidden w-64 border-r bg-sidebar text-sidebar-foreground md:block">
        <SidebarContent />
      </aside>
      <div className="flex min-h-dvh flex-col md:pl-64">
        <header className="sticky top-0 z-40 flex h-16 items-center gap-3 border-b bg-background/95 px-4 backdrop-blur md:px-8">
          <Sheet open={menuOpen} onOpenChange={setMenuOpen}>
            <SheetTrigger asChild>
              <Button variant="ghost" size="icon" className="md:hidden" aria-label="Open menu">
                <MenuIcon aria-hidden="true" />
              </Button>
            </SheetTrigger>
            <SheetContent
              closeLabel="Close menu"
              onCloseAutoFocus={(event) => {
                if (!closedByNavigation.current) return;
                closedByNavigation.current = false;
                event.preventDefault();
                main.current?.focus();
              }}
            >
              <SheetTitle>Menu</SheetTitle>
              <SheetDescription>Navigation and project</SheetDescription>
              <SidebarContent
                onNavigate={() => {
                  closedByNavigation.current = true;
                  setMenuOpen(false);
                }}
              />
            </SheetContent>
          </Sheet>
          <SearchBar className="max-w-xl flex-1" />
          <div className="ml-auto">
            <UserMenu me={me} />
          </div>
        </header>
        <main
          id="main"
          ref={main}
          tabIndex={-1}
          className="mx-auto w-full max-w-6xl flex-1 px-4 py-8 focus:outline-none md:px-8"
        >
          <Outlet />
        </main>
      </div>
    </div>
  );
}

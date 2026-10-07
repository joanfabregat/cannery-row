import { SearchIcon } from "lucide-react";
import { type SyntheticEvent, useEffect, useId, useRef, useState } from "react";
import { useLocation, useNavigate, useSearchParams } from "react-router";

import { Input } from "@/components/ui/input";
import { refTarget } from "@/lib/refs";
import { cn } from "@/lib/utils";

function isTyping(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName);
}

/** An open menu or dialog owns the keyboard (its own type-ahead, for example). */
function overlayOpen(): boolean {
  return document.querySelector('[role="menu"], [role="dialog"], [aria-modal="true"]') !== null;
}

/** A plain "/" typed outside any field, menu or dialog. */
function isSearchShortcut(event: KeyboardEvent): boolean {
  return (
    event.key === "/" &&
    !event.altKey &&
    !event.ctrlKey &&
    !event.metaKey &&
    !event.isComposing &&
    !event.defaultPrevented &&
    !isTyping(event.target) &&
    !overlayOpen()
  );
}

/** The global search bar, on every page. Press "/" to focus it. */
export function SearchBar({ className }: { className?: string }) {
  const id = useId();
  const input = useRef<HTMLInputElement>(null);
  const navigate = useNavigate();
  const location = useLocation();
  const [params] = useSearchParams();
  const current = location.pathname === "/search" ? (params.get("q") ?? "") : "";
  const [query, setQuery] = useState(current);
  // Follow the address when it changes (a new search, or leaving the search page).
  const [shown, setShown] = useState(current);
  if (shown !== current) {
    setShown(current);
    setQuery(current);
  }

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (isSearchShortcut(event)) {
        event.preventDefault();
        input.current?.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
    };
  }, []);

  const submit = (event: SyntheticEvent) => {
    event.preventDefault();
    const q = query.trim();
    // "#12", "slug#12" or "#12.3" opens that hypothesis or attempt directly.
    const target = refTarget(q);
    if (target !== null) {
      setQuery("");
      void navigate(target);
      return;
    }
    void navigate(q ? `/search?q=${encodeURIComponent(q)}` : "/search");
  };

  return (
    <form role="search" onSubmit={submit} className={cn("relative", className)}>
      <label htmlFor={id} className="sr-only">
        Search
      </label>
      <SearchIcon
        className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground"
        aria-hidden="true"
      />
      <Input
        ref={input}
        id={id}
        type="search"
        value={query}
        onChange={(event) => {
          setQuery(event.target.value);
        }}
        placeholder="Search, or go to #12"
        aria-keyshortcuts="/"
        className="pl-9"
      />
    </form>
  );
}

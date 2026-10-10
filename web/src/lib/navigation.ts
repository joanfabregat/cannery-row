import { useLocation } from "react-router";

/** A page's notice after an action ("Your decision was recorded."), from navigation state. */
export function useNotice(): string | null {
  const { state } = useLocation() as { state: unknown };
  if (typeof state === "object" && state !== null && "notice" in state) {
    const notice = state.notice;
    return typeof notice === "string" ? notice : null;
  }
  return null;
}

/** A unit number or attempt sequence from the address, or null. */
export function parseNumber(value: string | undefined): number | null {
  if (value === undefined || !/^[1-9][0-9]{0,8}$/.test(value)) return null;
  return Number(value);
}

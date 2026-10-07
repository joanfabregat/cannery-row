import type { Token } from "@/api/types";

/** As the backend: 1 to 100 printable characters, no ";". */
export const MAX_TOKEN_NAME = 100;

/** What is wrong with a token name, in plain words, or null. */
export function tokenNameProblem(name: string): string | null {
  const trimmed = name.trim();
  if (!trimmed) return "Give the token a name.";
  if (trimmed.length > MAX_TOKEN_NAME) return `At most ${String(MAX_TOKEN_NAME)} characters.`;
  // eslint-disable-next-line no-control-regex -- matching control characters is the point
  if (/[\u0000-\u001f\u007f-\u009f]/.test(trimmed) || trimmed.includes(";")) {
    return "Letters, digits, spaces and punctuation only, without “;”.";
  }
  return null;
}

export function tokenStatus(token: Token, now = Date.now()): "active" | "revoked" | "expired" {
  if (token.revoked_at) return "revoked";
  return new Date(token.expires_at).getTime() <= now ? "expired" : "active";
}

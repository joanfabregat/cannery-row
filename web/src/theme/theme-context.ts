import { createContext, useContext } from "react";

export type ThemeChoice = "light" | "dark" | "system";

/** Also read by public/theme-init.js before the first paint. */
export const THEME_STORAGE_KEY = "cannery-row.theme";

export interface ThemeState {
  choice: ThemeChoice;
  setChoice: (choice: ThemeChoice) => void;
}

export const ThemeContext = createContext<ThemeState | null>(null);

export function useTheme(): ThemeState {
  const state = useContext(ThemeContext);
  if (state === null) throw new Error("useTheme needs a ThemeProvider");
  return state;
}

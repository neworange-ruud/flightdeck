"use client";

import { ThemeProvider } from "next-themes";
import type { ReactNode } from "react";

import type { DocsSiteConfig } from "../types";

export function DocsThemeProvider({ children, siteConfig }: { children: ReactNode; siteConfig: DocsSiteConfig }) {
  return (
    <ThemeProvider attribute="data-theme" defaultTheme={siteConfig.theme.defaultColorMode} disableTransitionOnChange enableSystem themes={["light", "dark"]}>
      {children}
    </ThemeProvider>
  );
}

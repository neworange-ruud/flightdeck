import type { Metadata } from "next";
import { DocsThemeProvider, loadSiteConfig } from "@flightdeck/docs-platform";
import type { ReactNode } from "react";

import { siteDefinition } from "@/site-definition";

import "./globals.css";

export const metadata: Metadata = {
  title: "Flightdeck",
  description: "Orchestrate local AI coding agents in parallel.",
};

export default async function RootLayout({ children }: { children: ReactNode }) {
  const siteConfig = await loadSiteConfig(siteDefinition);

  return (
    <html lang="en" suppressHydrationWarning>
      <body>
        <DocsThemeProvider siteConfig={siteConfig}>{children}</DocsThemeProvider>
      </body>
    </html>
  );
}

export const APPROVED_MDX_COMPONENTS = [
  "Callout",
  "CardGrid",
  "Card",
  "Badge",
] as const;

export type DocsMdxComponentName = (typeof APPROVED_MDX_COMPONENTS)[number];

export interface DocsSiteDefinition {
  siteDir: string;
}

export interface DocsSiteConfig {
  siteTitle: string;
  repositoryUrl?: string;
  contentRoot: string;
  mountPath: string;
  theme: { defaultColorMode: "light" | "dark" | "system"; allowToggle: boolean };
  search: { enabled: boolean };
  navigation: { collapsible: boolean };
  mdxComponents: { enabled: DocsMdxComponentName[] };
  codeBlocks: { lineNumbers: boolean | "optional"; copyButton: boolean };
}

export interface DocsPage {
  body: string;
  headings: Array<{ depth: 2 | 3; id: string; value: string }>;
  isIndex: boolean;
  route: string;
  slugSegments: string[];
  textContent: string;
  title: string;
  frontmatter: { label?: string; order?: number; icon?: string; hidden: boolean };
}

export interface DocsNavigationItem {
  label: string;
  route: string;
}

export interface DocsSiteData {
  navigation: DocsNavigationItem[];
  pages: DocsPage[];
  pagesByRoute: Map<string, DocsPage>;
  siteConfig: DocsSiteConfig;
}

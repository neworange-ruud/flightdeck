import { readFile } from "node:fs/promises";
import path from "node:path";

import YAML from "yaml";
import { z } from "zod";

import { APPROVED_MDX_COMPONENTS, type DocsSiteConfig, type DocsSiteDefinition } from "./types";

const schema = z.object({
  siteTitle: z.string().min(1),
  repositoryUrl: z.string().url().optional(),
  contentRoot: z.string().min(1).optional(),
  mountPath: z.string().regex(/^\//).optional(),
  theme: z.object({
    defaultColorMode: z.enum(["light", "dark", "system"]).optional(),
    allowToggle: z.boolean().optional(),
  }).optional(),
  search: z.object({ enabled: z.boolean().optional() }).optional(),
  navigation: z.object({ collapsible: z.boolean().optional() }).optional(),
  mdxComponents: z.object({ enabled: z.array(z.enum(APPROVED_MDX_COMPONENTS)).optional() }).optional(),
  codeBlocks: z.object({
    lineNumbers: z.union([z.boolean(), z.literal("optional")]).optional(),
    copyButton: z.boolean().optional(),
  }).optional(),
}).strict();

export function defineDocsSite(siteDir: string): DocsSiteDefinition {
  return { siteDir };
}

export async function loadSiteConfig(site: DocsSiteDefinition | string): Promise<DocsSiteConfig> {
  const siteDir = typeof site === "string" ? site : site.siteDir;
  const source = await readFile(path.join(siteDir, "docs.config.yaml"), "utf8");
  const config = schema.parse(YAML.parse(source) ?? {});
  const mountPath = (config.mountPath ?? "/").replace(/\/$/, "") || "/";

  return {
    siteTitle: config.siteTitle,
    repositoryUrl: config.repositoryUrl,
    contentRoot: path.resolve(siteDir, config.contentRoot ?? "content"),
    mountPath,
    theme: {
      defaultColorMode: config.theme?.defaultColorMode ?? "system",
      allowToggle: config.theme?.allowToggle ?? true,
    },
    search: { enabled: config.search?.enabled ?? true },
    navigation: { collapsible: config.navigation?.collapsible ?? true },
    mdxComponents: { enabled: config.mdxComponents?.enabled ?? [...APPROVED_MDX_COMPONENTS] },
    codeBlocks: {
      lineNumbers: config.codeBlocks?.lineNumbers ?? false,
      copyButton: config.codeBlocks?.copyButton ?? true,
    },
  };
}

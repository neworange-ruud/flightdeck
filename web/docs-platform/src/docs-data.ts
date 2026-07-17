import { readFile } from "node:fs/promises";
import path from "node:path";

import fg from "fast-glob";
import GithubSlugger from "github-slugger";
import matter from "gray-matter";
import { toString } from "mdast-util-to-string";
import remarkGfm from "remark-gfm";
import remarkMdx from "remark-mdx";
import remarkParse from "remark-parse";
import { unified } from "unified";
import { visit } from "unist-util-visit";
import { z } from "zod";

import { toRoute } from "./route-utils";
import { loadSiteConfig } from "./site-config";
import type { DocsPage, DocsSiteData, DocsSiteDefinition } from "./types";

const frontmatterSchema = z.object({
  label: z.string().min(1).optional(),
  order: z.number().int().optional(),
  icon: z.string().min(1).optional(),
  hidden: z.boolean().optional(),
}).strict();

function analyzeBody(body: string, fileLabel: string, enabledComponents: Set<string>) {
  const tree = unified().use(remarkParse).use(remarkGfm).use(remarkMdx).parse(body);
  const slugger = new GithubSlugger();
  const headings: DocsPage["headings"] = [];
  const text: string[] = [];
  let title: string | undefined;

  visit(tree, (node: { type?: string; name?: string | null; depth?: number }) => {
    if (node.type === "mdxjsEsm" || node.type === "html") {
      throw new Error(`Unsupported MDX content in ${fileLabel}.`);
    }

    if (node.type === "mdxJsxFlowElement" || node.type === "mdxJsxTextElement") {
      if (!node.name || !enabledComponents.has(node.name)) {
        throw new Error(`Unsupported MDX component \`${node.name ?? "fragment"}\` in ${fileLabel}.`);
      }
    }

    if (node.type === "heading") {
      const value = toString(node).replace(/\s+/g, " ").trim();
      if (node.depth === 1 && !title) title = value;
      if (value && (node.depth === 2 || node.depth === 3)) {
        headings.push({ depth: node.depth, id: slugger.slug(value), value });
      }
    }
  });

  visit(tree, ["paragraph", "heading", "listItem"], (node) => {
    const value = toString(node).replace(/\s+/g, " ").trim();
    if (value) text.push(value);
  });

  return { headings, textContent: text.join("\n"), title };
}

export async function loadDocsTree(site: DocsSiteDefinition | string): Promise<DocsSiteData> {
  const siteConfig = await loadSiteConfig(site);
  const files = await fg("**/*.mdx", { absolute: true, cwd: siteConfig.contentRoot, onlyFiles: true });
  const pages = await Promise.all(files.sort().map(async (filePath) => {
    const source = await readFile(filePath, "utf8");
    const parsed = matter(source);
    const frontmatter = frontmatterSchema.parse(parsed.data ?? {});
    const relativePath = path.relative(siteConfig.contentRoot, filePath).replace(/\\/g, "/");
    const parsedPath = path.posix.parse(relativePath);
    const isIndex = parsedPath.name === "index";
    const folderSegments = parsedPath.dir ? parsedPath.dir.split("/").filter(Boolean) : [];
    const slugSegments = isIndex ? folderSegments : [...folderSegments, parsedPath.name];
    const analysis = analyzeBody(parsed.content, relativePath, new Set(siteConfig.mdxComponents.enabled));

    return {
      body: parsed.content,
      headings: analysis.headings,
      isIndex,
      route: toRoute(slugSegments, siteConfig.mountPath),
      slugSegments,
      textContent: analysis.textContent,
      title: analysis.title ?? frontmatter.label ?? parsedPath.name,
      frontmatter: { ...frontmatter, hidden: frontmatter.hidden ?? false },
    } satisfies DocsPage;
  }));
  const visiblePages = pages.filter((page) => !page.frontmatter.hidden);
  const navigation = visiblePages
    .map((page) => ({ label: page.frontmatter.label ?? page.title, route: page.route, order: page.frontmatter.order }))
    .sort((left, right) => (left.order ?? Infinity) - (right.order ?? Infinity) || left.label.localeCompare(right.label));

  return {
    navigation: navigation.map(({ label, route }) => ({ label, route })),
    pages,
    pagesByRoute: new Map(pages.map((page) => [page.route, page])),
    siteConfig,
  };
}

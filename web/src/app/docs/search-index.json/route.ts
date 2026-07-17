import { createDocsApp } from "@flightdeck/docs-platform";

import { siteDefinition } from "@/site-definition";

const docsApp = createDocsApp(siteDefinition);

export const dynamic = "force-static";
export const GET = docsApp.SearchIndexRoute;

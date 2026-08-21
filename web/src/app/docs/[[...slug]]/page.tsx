import { createDocsApp } from "@flightdeck/docs-platform";

import { siteDefinition } from "@/site-definition";

const docsApp = createDocsApp(siteDefinition);

export const dynamicParams = false;
export const generateMetadata = docsApp.generateMetadata;
export const generateStaticParams = docsApp.generateStaticParams;

export default docsApp.Page;

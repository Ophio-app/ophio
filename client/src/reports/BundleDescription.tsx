// SPDX-License-Identifier: Apache-2.0
import { describeBundle } from "../diagnostics/bundle";
import type { ReportBundle } from "../protocol/types";

export function BundleDescription({ bundle }: { bundle: ReportBundle }) {
  return <div className="report-bundle">{describeBundle(bundle).map(section => <details key={section.label} open>
    <summary>{section.label}</summary>
    <dl>{section.fields.map(field => <div key={JSON.stringify(field.path)}>
      {/* A field that is the whole section, like the report format, is named by its section. */}
      <dt className={field.path.length > 1 ? undefined : "visually-hidden"}>{field.path.length > 1 ? field.path.slice(1).join(" › ").replaceAll("_", " ") : section.label}</dt>
      <dd>{field.path[0] === "screenshot" && field.path[1] === "data"
        ? <details><summary>Image bytes (base64)</summary><code>{String(field.value)}</code></details>
        : <code>{typeof field.value === "string" ? field.value : JSON.stringify(field.value)}</code>}</dd>
    </div>)}</dl>
  </details>)}</div>;
}

// SPDX-License-Identifier: Apache-2.0

import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
// The app-wide styles, imported here only and in this order; scripts/css-order-check.mjs
// says why.
import "./design/tokens.css";
import "./design/base.css";
import "./design/components.css";
import "./shell/shell.css";
import "./screens/welcome/welcome.css";
import "./screens/talk/talk.css";
import "./design/material.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { crashes, DiagnosticsBoundary } from "./diagnostics/crashes";
import { trail } from "./diagnostics/trail";
import { reports } from "./diagnostics/reports";
import { App } from "./app/App";
import { apply } from "./app/prefs";
import { PRODUCT_NAME } from "./brand";
import { startTouch } from "./design/touch";

document.title = PRODUCT_NAME;
crashes.install();
trail.followLifecycle();
reports.follow();
apply();
startTouch();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <DiagnosticsBoundary><App /></DiagnosticsBoundary>
  </StrictMode>,
);

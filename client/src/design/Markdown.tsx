// SPDX-License-Identifier: Apache-2.0
//
// Safe Markdown rendering: no raw HTML, no remote images fetched implicitly,
// links open outside the app only when tapped.

import { createContext, memo, useContext, type ReactNode } from "react";
import ReactMarkdown, { defaultUrlTransform, type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { splitFrontMatter } from "./frontMatter";
import { remarkWindowsFileLinks } from "./windowsFileLinks";

const LinkTo = createContext<((href: string) => void) | undefined>(undefined);

function Link({ href, children }: { href?: string; children?: ReactNode }) {
  const onLink = useContext(LinkTo);
  return (
    <a
      href={href}
      onClick={(e) => {
        e.preventDefault();
        if (href && onLink) onLink(href);
      }}
      rel="noreferrer noopener"
    >
      {children}
    </a>
  );
}

// Made once. A component made on every render is a new kind of element to
// React, so each code block and table would be built again whenever the
// screen around it changed, and snap back to where it started scrolling.
const components: Components = {
  a: ({ href, children }) => <Link href={href}>{children}</Link>,
  img: ({ alt, src }) => (
    <span className="md__img">
      [{alt || "image"}
      {typeof src === "string" && src ? `: ${src}` : ""}]
    </span>
  ),
  table: ({ children }) => (
    <div className="md__table" tabIndex={0} role="region" aria-label="Table">
      <table>{children}</table>
    </div>
  ),
  pre: ({ children }) => (
    <pre className="md__pre" tabIndex={0}>
      {children}
    </pre>
  ),
  // Text shown on a screen sits under the screen's own title, so its
  // headings start a level down, each looking as written.
  h1: ({ children }) => <h2 className="md__h1">{children}</h2>,
  h2: ({ children }) => <h3 className="md__h2">{children}</h3>,
  h3: ({ children }) => <h4 className="md__h3">{children}</h4>,
  h4: ({ children }) => <h5 className="md__h4">{children}</h5>,
  h5: ({ children }) => <h6 className="md__h5">{children}</h6>,
  h6: ({ children }) => <h6 className="md__h6">{children}</h6>,
};

const urlTransform = (url: string) => {
  // A Windows drive is not a URL protocol. Markdown encodes backslashes as
  // %5C; restore them so the project file handler can recognize the drive.
  if (/^[a-z]:(?:[\\/]|%5c)/i.test(url)) return url.replace(/%5c/gi, "\\");
  // `README.md:12` cites a line in a file, but react-markdown takes
  // `README.md:` for an unknown protocol. Keep a filename with a numeric
  // line suffix; every other address goes through the usual check.
  return /^[^:/?#\\]+\.[^:/?#\\]+:\d+(?:[:-]\d+)?$/.test(url) ? url : defaultUrlTransform(url);
};

/** `document` shows leading YAML front matter as properties; leave it off for
 * chat and summaries, where a leading `---` is ordinary Markdown. */
export function Markdown({
  onLink,
  ...rest
}: {
  text: string;
  onLink?: (href: string) => void;
  className?: string;
  document?: boolean;
}) {
  return (
    <LinkTo.Provider value={onLink}>
      <Rendered {...rest} />
    </LinkTo.Provider>
  );
}

const Rendered = memo(function Rendered({
  text,
  className,
  document = false,
}: {
  text: string;
  className?: string;
  document?: boolean;
}) {
  const { front, body } = document
    ? splitFrontMatter(text)
    : { front: null, body: text };
  return (
    <div className={["md", className].filter(Boolean).join(" ")}>
      {front && (
        <section className="md__front" aria-label="File details">
          <p className="md__front-label">File details</p>
          {front.fields ? (
            <dl>
              {front.fields.map(([key, value], i) => (
                <div key={i}>
                  <dt>{key}</dt>
                  <dd>{value}</dd>
                </div>
              ))}
            </dl>
          ) : (
            <pre className="md__front-raw" tabIndex={0}>
              {front.raw}
            </pre>
          )}
        </section>
      )}
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkWindowsFileLinks]}
        skipHtml
        components={components}
        urlTransform={urlTransform}
      >
        {body}
      </ReactMarkdown>
    </div>
  );
});
